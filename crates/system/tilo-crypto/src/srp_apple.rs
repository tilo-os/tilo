//! SRP-6a as Apple's `idmsa` sign-in speaks it.
//!
//! RFC 5054's 2048-bit group with `g = 2` and SHA-256, plus Apple's
//! deviations: the password is first stretched with PBKDF2 (`s2k` over the
//! raw SHA-256 of the password, `s2k_fo` over its hex), the username is left
//! out of `x`, `k = H(N | pad(g))`, and every group element is padded to 256
//! bytes in the proofs.
//!
//! The client never sends the password or anything derived from it that is
//! not a zero-knowledge proof. The arithmetic is `crypto-bigint`'s
//! fixed-width Montgomery form: every exponentiation by a secret (the
//! ephemeral `a`, the password-derived `x`, and `a + u·x`) runs
//! [`FixedMontyForm::pow_bounded_exp`] with a fixed exponent bound, so its
//! time does not depend on the exponent's value, and the reduction and
//! subtraction steps never branch on secrets. Only the server's public `B`
//! and the public scrambler `u` are checked with branches. Every secret
//! integer and byte buffer is wiped when dropped.

use std::sync::OnceLock;

use crypto_bigint::modular::{FixedMontyForm, FixedMontyParams};
use crypto_bigint::{Odd, U256, U768, U2048};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

/// Bytes of a padded group element.
pub const ELEMENT_LEN: usize = 256;

const LIMBS: usize = U2048::LIMBS;
const WIDE: usize = U768::LIMBS;

type Element = FixedMontyForm<LIMBS>;

/// RFC 5054 appendix A, 2048-bit group.
const N: Odd<U2048> = Odd::<U2048>::from_be_hex(concat!(
    "AC6BDB41324A9A9BF166DE5E1389582FAF72B6651987EE07FC3192943DB56050",
    "A37329CBB4A099ED8193E0757767A13DD52312AB4B03310DCD7F48A9DA04FD50",
    "E8083969EDB767B0CF6095179A163AB3661A05FBD5FAAAE82918A9962F0B93B8",
    "55F97993EC975EEAA80D740ADBF4FF747359D041D5C33EA71D281E446B14773B",
    "CA97B43A23FB801676BD207A436C6481F1D2B9078717461A5B9D32E688F87748",
    "544523B524B0D57D5EA77A2775D2ECFA032CFBDBF52FB37861602790",
    "04E57AE6AF874E7303CE53299CCC041C7BC308D82A5698F3A8D0C38271AE35F8",
    "E9DBFBB694B5C803D89F7AE435DE236D525F54759B65E372FCD68EF20FA7111F",
    "9E4AFF73",
));

/// The bound for exponents that are SHA-256 outputs or 256-bit randomness
/// (`a`, `b`, `x`, `u`).
const SHORT_EXPONENT_BITS: u32 = 256;

/// The bound for `a + u·x`, which is below `2^513`.
const LONG_EXPONENT_BITS: u32 = 768;

/// Why a sign-in challenge could not be answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrpError {
    /// The server offered a password protocol other than `s2k`/`s2k_fo`.
    UnsupportedProtocol,
    /// The server's public value is degenerate (0 mod N) or longer than a
    /// group element, or the scrambler is zero.
    InvalidChallenge,
    /// The operating system could not supply randomness.
    Randomness,
}

impl std::fmt::Display for SrpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedProtocol => "unsupported SRP password protocol",
            Self::InvalidChallenge => "degenerate SRP challenge",
            Self::Randomness => "the operating system could not supply randomness",
        })
    }
}

impl std::error::Error for SrpError {}

/// The proofs sent to `/signin/complete`: the client's `M1` and the `M2` it
/// expects the server to answer with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Proofs {
    pub m1: [u8; 32],
    pub m2: [u8; 32],
}

struct Group {
    params: FixedMontyParams<LIMBS>,
    g: Element,
    /// `k = H(N | pad(g))`, in Montgomery form.
    k: Element,
    /// `H(pad(g)) XOR H(N)`, the first input of `M1`.
    hg_xor_hn: [u8; 32],
}

fn group() -> &'static Group {
    static GROUP: OnceLock<Group> = OnceLock::new();
    GROUP.get_or_init(|| {
        // N is public: its parameters may be computed in variable time.
        let params = FixedMontyParams::new_vartime(N);
        let g = U2048::from_u8(2);
        let n_bytes = to_bytes(N.as_ref());
        let g_bytes = to_bytes(&g);
        let k = from_digest(&hash(&[&n_bytes, &g_bytes]));
        let mut hg_xor_hn = hash(&[&g_bytes]);
        for (out, n) in hg_xor_hn.iter_mut().zip(hash(&[&n_bytes])) {
            *out ^= n;
        }
        Group {
            params,
            g: Element::new(&g, &params),
            k: Element::new(&k, &params),
            hg_xor_hn,
        }
    })
}

/// SHA-256 of the concatenation of `parts`.
fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// A digest as a 2048-bit integer.
fn from_digest(digest: &[u8; 32]) -> U2048 {
    U256::from_be_slice(digest).resize()
}

/// A group element as the 256-byte big-endian string the proofs hash.
fn to_bytes(value: &U2048) -> [u8; ELEMENT_LEN] {
    let mut out = [0_u8; ELEMENT_LEN];
    for (chunk, limb) in out
        .rchunks_exact_mut(size_of::<crypto_bigint::Word>())
        .zip(value.as_limbs())
    {
        chunk.copy_from_slice(&limb.0.to_be_bytes());
    }
    out
}

/// A public group element as the server sent it, padded to 256 bytes.
/// Longer encodings (after leading zeros) are refused.
fn padded(value: &[u8]) -> Result<[u8; ELEMENT_LEN], SrpError> {
    let start = value
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(value.len());
    let significant = value.get(start..).unwrap_or_default();
    let offset = ELEMENT_LEN
        .checked_sub(significant.len())
        .ok_or(SrpError::InvalidChallenge)?;
    let mut out = [0_u8; ELEMENT_LEN];
    out.get_mut(offset..)
        .ok_or(SrpError::InvalidChallenge)?
        .copy_from_slice(significant);
    Ok(out)
}

/// Apple's password stretching: PBKDF2-HMAC-SHA256 over SHA-256 of the
/// password (`s2k`) or its lowercase hex (`s2k_fo`).
///
/// # Errors
/// [`SrpError::UnsupportedProtocol`] for any other protocol.
pub fn derive_password(
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
fn private_key(salt: &[u8], derived: &[u8; 32]) -> Zeroizing<U256> {
    let inner = Zeroizing::new(hash(&[b":", derived]));
    let digest = Zeroizing::new(hash(&[salt, inner.as_slice()]));
    Zeroizing::new(U256::from_be_slice(digest.as_slice()))
}

/// `u = H(pad(A) | pad(B))`, public.
fn scrambler(a_pub: &[u8; ELEMENT_LEN], b_pub: &[u8; ELEMENT_LEN]) -> U256 {
    U256::from_be_slice(&hash(&[a_pub, b_pub]))
}

/// `M1 = H(H(pad g) XOR H(N) | H(username) | salt | pad A | pad B | K)` and
/// `M2 = H(pad A | M1 | K)`.
fn proofs(
    username: &str,
    salt: &[u8],
    a_pub: &[u8; ELEMENT_LEN],
    b_pub: &[u8; ELEMENT_LEN],
    session_key: &[u8; 32],
) -> Proofs {
    let hi = hash(&[username.as_bytes()]);
    let m1 = hash(&[&group().hg_xor_hn, &hi, salt, a_pub, b_pub, session_key]);
    let m2 = hash(&[a_pub, &m1, session_key]);
    Proofs { m1, m2 }
}

/// `K = H(pad(S))`.
fn session_key(shared: &Element) -> Zeroizing<[u8; 32]> {
    let shared = Zeroizing::new(shared.retrieve());
    let bytes = Zeroizing::new(to_bytes(&shared));
    Zeroizing::new(hash(&[bytes.as_slice()]))
}

/// `g^exponent mod N` for a 256-bit secret exponent.
fn g_pow(exponent: &U256) -> Element {
    group().g.pow_bounded_exp(exponent, SHORT_EXPONENT_BITS)
}

/// The client half of one sign-in.
pub struct SrpClient {
    /// The ephemeral private exponent `a`, below `2^256`.
    secret: Zeroizing<U256>,
    /// `A = g^a mod N`, padded.
    public: [u8; ELEMENT_LEN],
}

impl std::fmt::Debug for SrpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SrpClient").finish_non_exhaustive()
    }
}

impl SrpClient {
    /// A client with a fresh 256-bit private exponent.
    ///
    /// # Errors
    /// [`SrpError::Randomness`] when the operating system has none.
    pub fn new() -> Result<Self, SrpError> {
        let mut bytes = Zeroizing::new([0_u8; 32]);
        getrandom::fill(bytes.as_mut_slice()).map_err(|_| SrpError::Randomness)?;
        Ok(Self::with_secret(&bytes))
    }

    /// A client with a chosen private exponent, for known-answer tests.
    #[doc(hidden)]
    #[must_use]
    pub fn with_secret(secret: &[u8; 32]) -> Self {
        let secret = Zeroizing::new(U256::from_be_slice(secret));
        let public = to_bytes(&g_pow(&secret).retrieve());
        Self { secret, public }
    }

    /// `A`, padded, for `/signin/init`.
    #[must_use]
    pub fn public(&self) -> [u8; ELEMENT_LEN] {
        self.public
    }

    /// Answer the server's challenge (`salt`, `B`) for `username` with the
    /// stretched password from [`derive_password`].
    ///
    /// # Errors
    /// [`SrpError::InvalidChallenge`] when `B` is 0 mod N or longer than a
    /// group element, or the scrambler is zero.
    pub fn respond(
        &self,
        username: &str,
        derived: &[u8; 32],
        salt: &[u8],
        server_public: &[u8],
    ) -> Result<Proofs, SrpError> {
        let group = group();
        let b_bytes = padded(server_public)?;
        // Montgomery conversion reduces B mod N. B is public.
        let b_pub = Element::new(&U2048::from_be_slice(&b_bytes), &group.params);
        if b_pub.retrieve().is_zero_vartime() {
            return Err(SrpError::InvalidChallenge);
        }
        let u = scrambler(&self.public, &b_bytes);
        if u.is_zero_vartime() {
            return Err(SrpError::InvalidChallenge);
        }
        let x = private_key(salt, derived);
        // S = (B - k·g^x)^(a + u·x) mod N.
        let gx = Zeroizing::new(g_pow(&x));
        let kgx = Zeroizing::new(group.k.mul(&gx));
        let base = Zeroizing::new(b_pub.sub(&kgx));
        let a_wide = Zeroizing::new(self.secret.resize::<WIDE>());
        let x_wide = Zeroizing::new(x.resize::<WIDE>());
        // u, x < 2^256: the product and the sum stay far below 2^768.
        let ux = Zeroizing::new(u.resize::<WIDE>().wrapping_mul(&*x_wide));
        let exponent = Zeroizing::new(a_wide.wrapping_add(&ux));
        let shared = Zeroizing::new(base.pow_bounded_exp(&*exponent, LONG_EXPONENT_BITS));
        let key = session_key(&shared);
        Ok(proofs(username, salt, &self.public, &b_bytes, &key))
    }
}

/// The server half, for test servers that impersonate Apple's `idmsa`.
#[cfg(feature = "srp-test-server")]
pub mod server {
    use super::{
        ELEMENT_LEN, Element, Proofs, SHORT_EXPONENT_BITS, SrpError, U256, U2048, Zeroizing,
        derive_password, g_pow, group, padded, private_key, proofs, scrambler, session_key,
        to_bytes,
    };

    /// The verifier `v = g^x` a server stores for `password`, padded.
    ///
    /// # Errors
    /// [`SrpError::UnsupportedProtocol`] for a protocol other than
    /// `s2k`/`s2k_fo`.
    pub fn verifier(
        password: &str,
        salt: &[u8],
        iterations: u32,
        protocol: &str,
    ) -> Result<[u8; ELEMENT_LEN], SrpError> {
        let derived = derive_password(password, salt, iterations, protocol)?;
        let x = private_key(salt, &derived);
        Ok(to_bytes(&g_pow(&x).retrieve()))
    }

    /// One server side of a sign-in: the ephemeral `b` and
    /// `B = k·v + g^b mod N`.
    pub struct ServerSession {
        secret: Zeroizing<U256>,
        verifier: Element,
        public: [u8; ELEMENT_LEN],
    }

    impl std::fmt::Debug for ServerSession {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ServerSession").finish_non_exhaustive()
        }
    }

    impl ServerSession {
        /// A session for `verifier` with a fresh 256-bit `b`.
        ///
        /// # Errors
        /// [`SrpError::Randomness`] when the operating system has none.
        pub fn new(verifier: &[u8; ELEMENT_LEN]) -> Result<Self, SrpError> {
            let mut bytes = Zeroizing::new([0_u8; 32]);
            getrandom::fill(bytes.as_mut_slice()).map_err(|_| SrpError::Randomness)?;
            Ok(Self::with_secret(verifier, &bytes))
        }

        /// A session with a chosen `b`, for known-answer tests.
        #[must_use]
        pub fn with_secret(verifier: &[u8; ELEMENT_LEN], secret: &[u8; 32]) -> Self {
            let group = group();
            let secret = Zeroizing::new(U256::from_be_slice(secret));
            let verifier = Element::new(&U2048::from_be_slice(verifier), &group.params);
            let public = group.k.mul(&verifier).add(&g_pow(&secret));
            Self {
                secret,
                verifier,
                public: to_bytes(&public.retrieve()),
            }
        }

        /// `B`, padded, for the `/signin/init` answer.
        #[must_use]
        pub fn public(&self) -> [u8; ELEMENT_LEN] {
            self.public
        }

        /// The proofs a client that knows the password sends with
        /// `client_public` (`A`) for `username`.
        ///
        /// # Errors
        /// [`SrpError::InvalidChallenge`] when `A` is longer than a group
        /// element or 0 mod N.
        pub fn expected(
            &self,
            username: &str,
            salt: &[u8],
            client_public: &[u8],
        ) -> Result<Proofs, SrpError> {
            let group = group();
            let a_bytes = padded(client_public)?;
            let a_pub = Element::new(&U2048::from_be_slice(&a_bytes), &group.params);
            if a_pub.retrieve().is_zero_vartime() {
                return Err(SrpError::InvalidChallenge);
            }
            let u = scrambler(&a_bytes, &self.public);
            // S = (A·v^u)^b mod N.
            let vu = Zeroizing::new(self.verifier.pow_bounded_exp(&u, SHORT_EXPONENT_BITS));
            let base = Zeroizing::new(a_pub.mul(&vu));
            let shared = Zeroizing::new(base.pow_bounded_exp(&*self.secret, SHORT_EXPONENT_BITS));
            let key = session_key(&shared);
            Ok(proofs(username, salt, &a_bytes, &self.public, &key))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct KnownAnswer {
        protocol: &'static str,
        b_pub: &'static str,
        m1: &'static str,
        m2: &'static str,
    }

    const KAT_PASSWORD: &str = "correct horse";
    const KAT_SALT: [u8; 16] = [0x22; 16];
    const KAT_USERNAME: &str = "ada@example.com";
    const KAT_A: &str = concat!(
        "24d1e3e550122e1dc571bcefd01f494de5ca82c5ff005ac469a843e5c5a2898b",
        "4c3ea0bac3b9b8e552cef73254dceb5496f05eb1d82a97523ce07a43e4268468",
        "328741403099f4f0f7a4f28c79a75d2d2b9c27744582063df5d31e5ff586fe1e",
        "0266151a23549e9b61d93c8b575d28b188d045f7b97511afb36d73e6f8f8bc19",
        "605ff47c2440fd378d4bb53580d81f01f6bd1c608d9def0b7fefe662b2d4a669",
        "dcaceba2a2d8b3979371c0b74027231060f640a922b4374190333c4a102c6e76",
        "0e5208f75c88b396af912509427875e9649ff390e3a19488157c1593cb951401",
        "ccd848fc4bf779f86e5c06cf66f9b2ce0e8fbd26c96ac9b4d4662155f89b5d0f",
    );
    const KNOWN_ANSWERS: [KnownAnswer; 2] = [
        KnownAnswer {
            protocol: "s2k",
            b_pub: concat!(
                "7e929f93d025663b903b239bfa14ac13d79f8a0e0d3c5aed417322f2eeb4bc88",
                "15f38e224422dad86c574fa8e79ff206a608c7252ca0a56aef6c4099ccd355ef",
                "52e13342b93e9a3a2dc0a06733516c9cbf6a7c868e4b8551828742bd1df79b3b",
                "c451b970fa0824ffb22c96a5dbaa4d7d3ef093ac2217c53d80a59ba5e3b78d75",
                "27a9f4d653f69a58eeac1459decaadd7ce56c5dbd1a21dddeef64a1475328d47",
                "2c86aa41a99526eed7d707636f6319a99405d84c087cf881c53d29eb8cca5aaa",
                "077580c88b4b20a3c33a0f84ab3d9603cbbb8a09d32db87102261653d330e798",
                "14fd05d3fe16c79335af1763adc2585096780dd89a0a3475f77bb96def3bbaee",
            ),
            m1: "609da1da8e3e8cf57a79117599acaef1f1e2f54a8358d66245b487d09371b9f7",
            m2: "f2a685b43eaea89f35da418823db608e57b05371fad6ddf70bad963289c46fcf",
        },
        KnownAnswer {
            protocol: "s2k_fo",
            b_pub: concat!(
                "830c7ea0e25834639bcda8634462d6fef0f77f4521fce94c95bb66e6cf03e8db",
                "9096a50ddcc99663dcaacc7cd238edf2af763e2969641354908c5bfe87fa1d29",
                "58d8f484677fb85d660f56359eff3d46ba82e1f3e5b3605573e77714078f85cd",
                "aea2fab447b7649d824aaa78357bc79c3413f902b0da9da89680abe6e37a0db0",
                "b148ad3a7cfaf83a9dd1900d87d422a8e83714f90e8b8078a5016e1d51cf9022",
                "9500c687766a2f3ebf3ea387ab5165a650bb99c0fa02790743bc00f2ef6377bc",
                "63a587ceae3b1372fe7c2d6ee156c2415d588eaccd1f40a507c58ab8e830a950",
                "30c0b6a11a4e5e54106f2a50cd58e40d160a95cb2f06f26565940dcc3131f46f",
            ),
            m1: "185a9221b74831f0488f3c482f36a59f9e0760a5e246405591788b174f9f1ad4",
            m2: "38d26d7ae00556a5faed7482f8b1c89b9ed3e9f3ad046904d015bb84fa3fbc13",
        },
    ];

    /// Known answers recorded from the `num-bigint` implementation this
    /// module replaced (cloud-source-icloud's `auth::srp`): `a = [0x11; 32]`,
    /// password `correct horse`, salt `[0x22; 16]`, 1000 iterations, and the
    /// `B` a server with `b = [0x33; 32]` sends for that password. Apple
    /// checks `M1` byte for byte, so these must never change.
    #[test]
    fn apple_srp_known_answers() {
        for answer in KNOWN_ANSWERS {
            let derived = derive_password(KAT_PASSWORD, &KAT_SALT, 1000, answer.protocol).unwrap();
            let client = SrpClient::with_secret(&[0x11; 32]);
            assert_eq!(hex::encode(client.public()), KAT_A);
            let b_pub = hex::decode(answer.b_pub).unwrap();
            let proofs = client
                .respond(KAT_USERNAME, &derived, &KAT_SALT, &b_pub)
                .unwrap();
            assert_eq!(hex::encode(proofs.m1), answer.m1, "{}", answer.protocol);
            assert_eq!(hex::encode(proofs.m2), answer.m2, "{}", answer.protocol);
        }
    }

    /// The test server sends the recorded `B` for the recorded `b`, and
    /// expects the recorded proofs.
    #[cfg(feature = "srp-test-server")]
    #[test]
    fn the_test_server_reproduces_the_known_answers() {
        for answer in KNOWN_ANSWERS {
            let verifier =
                server::verifier(KAT_PASSWORD, &KAT_SALT, 1000, answer.protocol).unwrap();
            let session = server::ServerSession::with_secret(&verifier, &[0x33; 32]);
            assert_eq!(hex::encode(session.public()), answer.b_pub);
            let a_pub = hex::decode(KAT_A).unwrap();
            let expected = session.expected(KAT_USERNAME, &KAT_SALT, &a_pub).unwrap();
            assert_eq!(hex::encode(expected.m1), answer.m1);
            assert_eq!(hex::encode(expected.m2), answer.m2);
        }
    }

    #[cfg(feature = "srp-test-server")]
    #[test]
    fn client_and_server_agree_on_the_proof() {
        let salt = b"0123456789abcdef";
        let derived = derive_password("correct horse", salt, 1000, "s2k").unwrap();
        let verifier = server::verifier("correct horse", salt, 1000, "s2k").unwrap();
        let session = server::ServerSession::with_secret(&verifier, &[7; 32]);

        let client = SrpClient::with_secret(&[3; 32]);
        let proofs = client
            .respond("ada@example.com", &derived, salt, &session.public())
            .unwrap();

        let expected = session
            .expected("ada@example.com", salt, &client.public())
            .unwrap();
        assert_eq!(proofs, expected);
    }

    #[cfg(feature = "srp-test-server")]
    #[test]
    fn a_wrong_password_yields_a_different_proof() {
        let salt = b"salt";
        let right = derive_password("right", salt, 10, "s2k").unwrap();
        let wrong = derive_password("wrong", salt, 10, "s2k").unwrap();
        let verifier = server::verifier("right", salt, 10, "s2k").unwrap();
        let session = server::ServerSession::with_secret(&verifier, &[9; 32]);
        let client = SrpClient::with_secret(&[5; 32]);
        let a = client
            .respond("u", &right, salt, &session.public())
            .unwrap();
        let b = client
            .respond("u", &wrong, salt, &session.public())
            .unwrap();
        assert_ne!(a.m1, b.m1);
        let expected = session.expected("u", salt, &client.public()).unwrap();
        assert_eq!(a, expected);
        assert_ne!(b, expected);
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
        let client = SrpClient::with_secret(&[1; 32]);
        let n = to_bytes(N.as_ref());
        assert_eq!(
            client.respond("u", &[0; 32], b"s", &n).err(),
            Some(SrpError::InvalidChallenge),
            "B = N is 0 mod N"
        );
        assert_eq!(
            client.respond("u", &[0; 32], b"s", &[0]).err(),
            Some(SrpError::InvalidChallenge)
        );
        let mut long = vec![1_u8];
        long.extend_from_slice(&n);
        assert_eq!(
            client.respond("u", &[0; 32], b"s", &long).err(),
            Some(SrpError::InvalidChallenge),
            "B longer than a group element"
        );
    }

    #[test]
    fn group_elements_pad_to_256_bytes() {
        let one = padded(&[1]).unwrap();
        assert_eq!(one.len(), ELEMENT_LEN);
        assert_eq!(one.last(), Some(&1));
        assert!(one.iter().rev().skip(1).all(|byte| *byte == 0));
        let n = to_bytes(N.as_ref());
        assert_eq!(n.first(), Some(&0xAC));
        let mut zero_led = vec![0_u8; 3];
        zero_led.extend_from_slice(&n);
        assert_eq!(
            padded(&zero_led).unwrap(),
            n,
            "leading zeros are not length"
        );
    }

    #[test]
    fn fresh_clients_draw_fresh_secrets() {
        let first = SrpClient::new().unwrap();
        let second = SrpClient::new().unwrap();
        assert_ne!(first.public(), second.public());
    }
}
