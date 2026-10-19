//! Cryptography of other applications' on-disk formats that Tilo must read.
//!
//! These are compatibility decoders, not sealing constructions Tilo chose:
//!
//! - Chromium's Linux `OSCrypt` cookie format uses `PBKDF2-HMAC-SHA1` with
//!   the fixed salt `saltysalt` and one iteration for its `v10`/`v11` keys,
//!   then AES-128-CBC with sixteen space bytes as the IV and PKCS#7 padding.
//! - Rclone's `obscure` password format is AES-256-CTR under a published
//!   constant key, with a random 16-byte IV before the ciphertext and
//!   URL-safe base64 around both. Rclone documents it as obfuscation, not
//!   secure custody; the caller reseals the plaintext in Tilo's own storage.
//!
//! Both live here because raw password-KDF, AES and CTR crates may only be
//! named by this crate ([`crate`]'s dependency boundary). Neither function
//! authenticates its input: the formats do not carry a tag. Callers validate
//! the application's own structure (a host hash, a URL, a length bound) and
//! never treat a successful decode as proof of integrity.
//!
//! The key material and plaintext are wiped on drop, and errors name only
//! the check that failed.

use cbc::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use ctr::cipher::StreamCipher;
use sha1::Sha1;
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Length of Chromium's `OSCrypt` AES-128 key.
pub const CHROMIUM_KEY_LEN: usize = 16;
/// Largest Chromium ciphertext this decoder accepts. Cookies are credentials,
/// not files, and the caller's format bound is the same.
pub const MAX_CHROMIUM_CIPHERTEXT: usize = 32 * 1024;
/// Largest rclone `obscure` string this decoder accepts.
pub const MAX_RCLONE_OBSCURED: usize = 16 * 1024;
/// Chromium derives its key from this fixed salt and one iteration.
const CHROMIUM_SALT: &[u8] = b"saltysalt";
/// Chromium's CBC initialisation vector is sixteen spaces.
const CHROMIUM_IV: [u8; 16] = [b' '; 16];
/// Rclone's published `obscure` key, in the order its source writes it.
const RCLONE_KEY: [u8; 32] = [
    0x9c, 0x93, 0x5b, 0x48, 0x73, 0x0a, 0x55, 0x4d, 0x6b, 0xfd, 0x7c, 0x63, 0xc8, 0x86, 0xa9, 0x2b,
    0xd3, 0x90, 0x19, 0x8e, 0xb8, 0x12, 0x8a, 0xfb, 0xf4, 0xde, 0x16, 0x2b, 0x8b, 0x95, 0xf6, 0x38,
];
/// Rclone's IV length before the ciphertext.
const RCLONE_IV_LEN: usize = 16;

/// The Chromium Linux `OSCrypt` key for `password`.
///
/// This is the compatibility KDF the format defines, not a policy for
/// storing a password; Tilo's own passwords go through
/// [`crate::envelope`].
#[must_use]
pub fn chromium_oscrypt_key(password: &[u8]) -> Zeroizing<[u8; CHROMIUM_KEY_LEN]> {
    let mut key = Zeroizing::new([0_u8; CHROMIUM_KEY_LEN]);
    pbkdf2::pbkdf2_hmac::<Sha1>(password, CHROMIUM_SALT, 1, key.as_mut_slice());
    key
}

/// Decrypt a Chromium `OSCrypt` AES-128-CBC ciphertext with `key`.
///
/// The returned bytes still carry the format's own structure: schema 24
/// prefixes the SHA-256 of the host key, which the caller checks.
///
/// # Errors
///
/// [`Error::Malformed`] for an empty, oversized or misaligned ciphertext or
/// bad padding, [`Error::InvalidKey`] if the cipher refuses its fixed-length key.
pub fn chromium_oscrypt_plaintext(
    key: &[u8; CHROMIUM_KEY_LEN],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    if ciphertext.is_empty()
        || ciphertext.len() > MAX_CHROMIUM_CIPHERTEXT
        || !ciphertext.len().is_multiple_of(16)
    {
        return Err(Error::Malformed("chromium ciphertext length"));
    }
    let decryptor = cbc::Decryptor::<aes::Aes128>::new_from_slices(key, &CHROMIUM_IV)
        .map_err(|_| Error::InvalidKey)?;
    // Decrypt in place in a wiped buffer: a bad final block must not leave the
    // earlier, correctly decrypted plaintext to the allocator.
    let mut plaintext = Zeroizing::new(ciphertext.to_vec());
    let length = decryptor
        .decrypt_padded::<Pkcs7>(plaintext.as_mut_slice())
        .map_err(|_| Error::Malformed("chromium padding"))?
        .len();
    plaintext.truncate(length);
    Ok(plaintext)
}

/// Decode the plaintext of an rclone `obscure` password.
///
/// The ciphertext is unauthenticated by design; the caller treats the result
/// as untrusted input and reseals it.
///
/// # Errors
///
/// [`Error::Malformed`] for a wrong base64 alphabet, an oversized string, a
/// ciphertext missing its IV, an empty password or non-UTF-8 plaintext.
pub fn rclone_obscure(encoded: &str) -> Result<Zeroizing<String>> {
    use base64::Engine as _;
    if encoded.len() > MAX_RCLONE_OBSCURED {
        return Err(Error::Malformed("rclone length"));
    }
    let mut bytes = Zeroizing::new(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| Error::Malformed("rclone base64"))?,
    );
    let (iv, ciphertext) = bytes
        .split_at_mut_checked(RCLONE_IV_LEN)
        .ok_or(Error::Malformed("rclone iv length"))?;
    if ciphertext.is_empty() {
        return Err(Error::Malformed("rclone ciphertext length"));
    }
    let mut cipher = ctr::Ctr128BE::<aes::Aes256>::new_from_slices(&RCLONE_KEY, iv)
        .map_err(|_| Error::InvalidKey)?;
    cipher
        .try_apply_keystream(ciphertext)
        .map_err(|_| Error::Seal)?;
    let password = std::str::from_utf8(ciphertext).map_err(|_| Error::Malformed("rclone utf-8"))?;
    if password.is_empty() {
        return Err(Error::Malformed("rclone password length"));
    }
    Ok(Zeroizing::new(password.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest as _;

    /// Generated independently with `hashlib.pbkdf2_hmac('sha1', password,
    /// b'saltysalt', 1, 16)`, then external AES-128-CBC with sixteen spaces
    /// as the IV. Schema 24 prefixes the UTF-8 host's SHA-256; schema 23 does
    /// not. The plaintext is a synthetic test value.
    const CHROMIUM_V11: &str = "ee96ec8095b1bf04a8f9fa1c5a7d45308ea6eb722925cccd5f79ba1ba5f754d71a47efde835ff2fd5375a1c507ea6a313def214c97d1c7f3a16e8eb9119fbd19";

    #[test]
    fn chromium_oscrypt_matches_an_independent_vector() {
        let key = chromium_oscrypt_key(b"synthetic-owner-password");
        assert_eq!(hex::encode(*key), "80e491d81e9a97ac6761136ccd850ae8");
        let plaintext =
            chromium_oscrypt_plaintext(&key, &hex::decode(CHROMIUM_V11).unwrap()).unwrap();
        let (host, value) = plaintext.split_at(32);
        assert_eq!(host, sha2::Sha256::digest(b"discord.com").as_slice());
        assert_eq!(value, b"synthetic-encrypted-session");
        assert_eq!(
            chromium_oscrypt_plaintext(
                &chromium_oscrypt_key(b"wrong"),
                &hex::decode(CHROMIUM_V11).unwrap()
            )
            .err(),
            Some(Error::Malformed("chromium padding"))
        );
        assert_eq!(
            chromium_oscrypt_plaintext(&key, &[0_u8; 15]).err(),
            Some(Error::Malformed("chromium ciphertext length"))
        );
    }

    /// Four AES blocks decrypt before the padding check reads the last one.
    /// Byte 47 is the final ciphertext byte before the last block; changing it
    /// flips that byte in the previous block's plaintext and the final padding
    /// byte (CBC), from a valid `0x05` to invalid zero. The error therefore
    /// comes from a decode that already produced three blocks of plaintext —
    /// the path the wiped in-place buffer exists for.
    #[test]
    fn a_multiblock_ciphertext_with_invalid_padding_is_refused() {
        let key = chromium_oscrypt_key(b"synthetic-owner-password");
        let mut ciphertext = hex::decode(CHROMIUM_V11).unwrap();
        assert_eq!(ciphertext.len(), 64, "four AES blocks");
        ciphertext[47] ^= 0x05;
        assert_eq!(
            chromium_oscrypt_plaintext(&key, &ciphertext).err(),
            Some(Error::Malformed("chromium padding"))
        );
    }

    #[test]
    fn rclone_obscure_matches_an_independent_vector() {
        assert_eq!(
            rclone_obscure("AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9")
                .unwrap()
                .as_str(),
            "fixture-app-password"
        );
        for invalid in [
            "not base64!",
            "AAECAwQFBgcICQoLDA0ODw",
            "AAECAwQFBgcICQoLDA0OD",
        ] {
            assert!(rclone_obscure(invalid).is_err(), "{invalid}");
        }
    }
}
