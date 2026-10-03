//! SRP-6a as Apple's `idmsa` sign-in speaks it.
//!
//! RFC 5054's 2048-bit group with `g = 2` and SHA-256, plus Apple's
//! deviations: the password is first stretched with PBKDF2 (`s2k` over the
//! raw SHA-256 of the password, `s2k_fo` over its hex), the username is left
//! out of `x`, and every group element is padded to 256 bytes in the proofs.
//!
//! The client never sends the password or anything derived from it that is
//! not a zero-knowledge proof. Byte buffers holding the stretched password
//! and session key are zeroized; the `BigUint` intermediates cannot be wiped
//! in place and live only for the one sign-in.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "arbitrary-precision BigUint arithmetic cannot overflow"
)]

use num_bigint::BigUint;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// RFC 5054 appendix A, 2048-bit group.
const N_HEX: &[u8] = b"AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC3192943DB56050\
A37329CBB4A099ED8193E0757767A13DD52312AB4B03310DCD7F48A9DA04FD50\
E8083969EDB767B0CF6095179A163AB3661A05FBD5FAAAE82918A9962F0B93B8\
55F97993EC975EEAA80D740ADBF4FF747359D041D5C33EA71D281E446B14773B\
CA97B43A23FB801676BD207A436C6481F1D2B9078717461A5B9D32E688F87748\
544523B524B0D57D5EA77A2775D2ECFA032CFBDBF52FB37861602790\
04E57AE6AF874E7303CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8\
E9DBFBB694B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F\
9E4AFF73";

/// Bytes of a padded group element.
const N_LEN: usize = 256;

/// Why a sign-in challenge could not be answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrpError {
    /// The server offered a password protocol other than `s2k`/`s2k_fo`.
    UnsupportedProtocol,
    /// The server's public value or the scrambler is degenerate (0 mod N).
    InvalidChallenge,
    /// The operating system could not supply randomness.
    Randomness,
}

pub(crate) struct Group {
    pub n: BigUint,
    pub g: BigUint,
    pub k: BigUint,
}

pub(crate) fn group() -> Group {
    let n = BigUint::parse_bytes(N_HEX, 16).unwrap_or_default();
    let g = BigUint::from(2_u32);
    let k = BigUint::from_bytes_be(&hash(&[&n.to_bytes_be(), &pad(&g)]));
    Group { n, g, k }
}

/// A group element as the 256-byte big-endian string the proofs hash.
pub(crate) fn pad(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    if bytes.len() >= N_LEN {
        return bytes;
    }
    let mut padded = vec![0_u8; N_LEN.saturating_sub(bytes.len())];
    padded.extend_from_slice(&bytes);
    padded
}

pub(crate) fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// Apple's password stretching: PBKDF2-HMAC-SHA256 over SHA-256 of the
/// password (`s2k`) or its lowercase hex (`s2k_fo`).
pub(crate) fn derive_password(
    password: &str,
    salt: &[u8],
    iterations: u32,
    protocol: &str,
) -> Result<Zeroizing<[u8; 32]>, SrpError> {
    let digest = Zeroizing::new(hash(&[password.as_bytes()]));
    let input: Zeroizing<Vec<u8>> = match protocol {
        "s2k" => Zeroizing::new(digest.to_vec()),
        "s2k_fo" => Zeroizing::new(hex::encode(digest.as_slice()).into_bytes()),
        _ => return Err(SrpError::UnsupportedProtocol),
    };
    let mut key = Zeroizing::new([0_u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha256>(&input, salt, iterations.max(1), key.as_mut_slice());
    Ok(key)
}

/// `x = H(salt | H(":" | derived))`: Apple leaves the username out.
pub(crate) fn private_key(salt: &[u8], derived: &[u8]) -> BigUint {
    let inner = Zeroizing::new(hash(&[b":", derived]));
    BigUint::from_bytes_be(&hash(&[salt, inner.as_slice()]))
}

/// `u = H(pad(A) | pad(B))`.
pub(crate) fn scrambler(a_pub: &BigUint, b_pub: &BigUint) -> BigUint {
    BigUint::from_bytes_be(&hash(&[&pad(a_pub), &pad(b_pub)]))
}

/// `M1 = H(H(pad g) XOR H(N) | H(username) | salt | pad A | pad B | K)`.
pub(crate) fn client_proof(
    group: &Group,
    username: &str,
    salt: &[u8],
    a_pub: &BigUint,
    b_pub: &BigUint,
    session_key: &[u8],
) -> [u8; 32] {
    let hg = hash(&[&pad(&group.g)]);
    let hn = hash(&[&group.n.to_bytes_be()]);
    let mut xor = [0_u8; 32];
    for ((out, g), n) in xor.iter_mut().zip(hg).zip(hn) {
        *out = g ^ n;
    }
    let hi = hash(&[username.as_bytes()]);
    hash(&[&xor, &hi, salt, &pad(a_pub), &pad(b_pub), session_key])
}

/// `M2 = H(pad A | M1 | K)`.
pub(crate) fn server_proof(a_pub: &BigUint, m1: &[u8], session_key: &[u8]) -> [u8; 32] {
    hash(&[&pad(a_pub), m1, session_key])
}

/// The client half of one sign-in.
pub struct Client {
    group: Group,
    secret: BigUint,
    public: BigUint,
}

/// The proofs sent to `/signin/complete`.
pub struct Proofs {
    pub m1: [u8; 32],
    pub m2: [u8; 32],
}

impl Client {
    /// A client with a fresh 256-bit private exponent.
    pub fn new() -> Result<Self, SrpError> {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        getrandom::fill(bytes.as_mut_slice()).map_err(|_| SrpError::Randomness)?;
        Ok(Self::with_secret(bytes.as_slice()))
    }

    pub(crate) fn with_secret(secret: &[u8]) -> Self {
        let group = group();
        let secret = BigUint::from_bytes_be(secret);
        let public = group.g.modpow(&secret, &group.n);
        Self {
            group,
            secret,
            public,
        }
    }

    /// `A`, padded, for `/signin/init`.
    pub fn public(&self) -> Vec<u8> {
        pad(&self.public)
    }

    /// Answer the server's challenge (`salt`, `B`) for `username` with the
    /// stretched password.
    pub fn respond(
        &self,
        username: &str,
        derived: &[u8],
        salt: &[u8],
        server_public: &[u8],
    ) -> Result<Proofs, SrpError> {
        let n = &self.group.n;
        let b_pub = BigUint::from_bytes_be(server_public);
        if (&b_pub % n) == BigUint::default() {
            return Err(SrpError::InvalidChallenge);
        }
        let u = scrambler(&self.public, &b_pub);
        if u == BigUint::default() {
            return Err(SrpError::InvalidChallenge);
        }
        let x = private_key(salt, derived);
        // S = (B - k·g^x)^(a + u·x) mod N, kept non-negative.
        let kgx = (&self.group.k * self.group.g.modpow(&x, n)) % n;
        let base = ((&b_pub % n) + n - kgx) % n;
        let exponent = &self.secret + &u * &x;
        let shared = base.modpow(&exponent, n);
        let session_key = Zeroizing::new(hash(&[&pad(&shared)]));
        let m1 = client_proof(
            &self.group,
            username,
            salt,
            &self.public,
            &b_pub,
            session_key.as_slice(),
        );
        let m2 = server_proof(&self.public, &m1, session_key.as_slice());
        Ok(Proofs { m1, m2 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The server side, as the fake sign-in computes it.
    fn server_session(
        verifier: &BigUint,
        secret: &BigUint,
        a_pub: &BigUint,
        b_pub: &BigUint,
    ) -> [u8; 32] {
        let g = group();
        let u = scrambler(a_pub, b_pub);
        let shared = (a_pub * verifier.modpow(&u, &g.n)).modpow(secret, &g.n);
        hash(&[&pad(&shared)])
    }

    #[test]
    fn client_and_server_agree_on_the_proof() {
        let g = group();
        let salt = b"0123456789abcdef";
        let derived = derive_password("correct horse", salt, 1000, "s2k").unwrap();
        let x = private_key(salt, derived.as_slice());
        let verifier = g.g.modpow(&x, &g.n);
        let b = BigUint::from_bytes_be(&[7; 32]);
        let b_pub = (&g.k * &verifier + g.g.modpow(&b, &g.n)) % &g.n;

        let client = Client::with_secret(&[3; 32]);
        let a_pub = BigUint::from_bytes_be(&client.public());
        let proofs = client
            .respond("ada@example.com", derived.as_slice(), salt, &pad(&b_pub))
            .unwrap();

        let key = server_session(&verifier, &b, &a_pub, &b_pub);
        let expected = client_proof(&g, "ada@example.com", salt, &a_pub, &b_pub, &key);
        assert_eq!(proofs.m1, expected);
        assert_eq!(proofs.m2, server_proof(&a_pub, &expected, &key));
    }

    #[test]
    fn a_wrong_password_yields_a_different_proof() {
        let g = group();
        let salt = b"salt";
        let right = derive_password("right", salt, 10, "s2k").unwrap();
        let wrong = derive_password("wrong", salt, 10, "s2k").unwrap();
        let verifier = g.g.modpow(&private_key(salt, right.as_slice()), &g.n);
        let b_pub = (&g.k * &verifier + g.g.modpow(&BigUint::from(9_u32), &g.n)) % &g.n;
        let client = Client::with_secret(&[5; 32]);
        let a = client
            .respond("u", right.as_slice(), salt, &pad(&b_pub))
            .unwrap();
        let b = client
            .respond("u", wrong.as_slice(), salt, &pad(&b_pub))
            .unwrap();
        assert_ne!(a.m1, b.m1);
    }

    #[test]
    fn protocols_stretch_differently_and_unknown_ones_are_refused() {
        let s2k = derive_password("pw", b"salt", 10, "s2k").unwrap();
        let s2k_fo = derive_password("pw", b"salt", 10, "s2k_fo").unwrap();
        assert_ne!(s2k.as_slice(), s2k_fo.as_slice());
        assert_eq!(
            derive_password("pw", b"salt", 10, "plain").err(),
            Some(SrpError::UnsupportedProtocol)
        );
    }

    #[test]
    fn degenerate_challenges_are_refused() {
        let g = group();
        let client = Client::with_secret(&[1; 32]);
        assert_eq!(
            client.respond("u", &[0; 32], b"s", &pad(&g.n)).err(),
            Some(SrpError::InvalidChallenge)
        );
        assert_eq!(
            client.respond("u", &[0; 32], b"s", &[0]).err(),
            Some(SrpError::InvalidChallenge)
        );
    }

    #[test]
    fn group_elements_pad_to_256_bytes() {
        assert_eq!(pad(&BigUint::from(1_u32)).len(), 256);
        assert_eq!(pad(&group().n).len(), 256);
    }
}
