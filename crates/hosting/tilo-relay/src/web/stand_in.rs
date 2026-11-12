//! A person's stand-in approver ([`tilo_relay_protocol::login_code`]): a key
//! this host holds for one person who set Tilo up through an agent, whose
//! root lives only in one browser tab until they keep it with a passkey.
//!
//! What the stand-in can do, and all it can do: certify a new device of
//! its own person, with member capabilities and a browser's lifetime, for
//! exactly the certification request that names an invite a device of that
//! person minted by claiming a sign-in code ([`super::login_codes`]). The
//! host signs with it nowhere else: not for another person, not for a
//! request nobody claimed, not for a second request naming the same invite,
//! not a host, a grant or another approver. It approves only while its
//! root-signed certificate (a stand-in capability, at most
//! [`tilo_relay_protocol::login_code::STAND_IN_LIFETIME_MS`]) is live and unrevoked, and the person's root
//! retires it when they first keep the root with a passkey: the host forgets
//! the key, and every device it certified before stays valid.
//!
//! Each key is kept in the host's state directory, readable by the host
//! alone, beside its certificate once the root sent it; at most
//! [`MAX_STAND_INS`] people have one on a host.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::certify::MEMBER_APPROVAL_SKEW_NS;
use tilo_relay_protocol::login_code::{StandInReply, StandInRequest};
use tilo_user_identity::{DeviceCert, RevocationList};

/// People with a stand-in on one host at most.
pub(crate) const MAX_STAND_INS: usize = 4096;

/// Why a stand-in request was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `400`.
    Malformed(&'static str),
    /// `403`.
    Forbidden(&'static str),
    /// `404`: no stand-in key for this person.
    Unknown,
    /// `503`.
    Full,
    /// `500`.
    Storage,
}

impl Refusal {
    pub(crate) const fn message(&self) -> &'static str {
        match self {
            Self::Malformed(message) | Self::Forbidden(message) => message,
            Self::Unknown => "this host keeps no stand-in for this person",
            Self::Full => "this host keeps as many stand-ins as it can",
            Self::Storage => "this host could not keep the stand-in",
        }
    }
}

/// One person's stand-in.
struct Kept {
    key: SigningKey,
    /// Its root-signed certificate, once kept.
    cert: Option<DeviceCert>,
}

/// The stand-ins a host keeps.
pub(crate) struct StandIns {
    dir: PathBuf,
    kept: Mutex<HashMap<[u8; 32], Kept>>,
}

fn hex32(text: &str) -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(text, &mut bytes).ok()?;
    Some(bytes)
}

fn signature(text: &str) -> Option<Signature> {
    let mut bytes = [0; 64];
    hex::decode_to_slice(text, &mut bytes).ok()?;
    Some(Signature::from_bytes(&bytes))
}

/// Check the root of `user` signed `op` at `signed_at_ns`, near `now_ns`.
fn root_signed(
    op: &str,
    user: &str,
    signed_at_ns: u64,
    signature_hex: &str,
    now_ns: u64,
) -> Result<[u8; 32], Refusal> {
    let user = hex32(user).ok_or(Refusal::Malformed("a person's root is 32 bytes hex"))?;
    let signature =
        signature(signature_hex).ok_or(Refusal::Malformed("a signature is 64 bytes hex"))?;
    if signed_at_ns.abs_diff(now_ns) > MEMBER_APPROVAL_SKEW_NS {
        return Err(Refusal::Forbidden(
            "the request is too old or from the future",
        ));
    }
    VerifyingKey::from_bytes(&user)
        .and_then(|root| {
            root.verify_strict(
                &StandInRequest::signed_message(op, &user, signed_at_ns),
                &signature,
            )
        })
        .map_err(|_| Refusal::Forbidden("the request is not signed by this person's root"))?;
    Ok(user)
}

/// Make this host the stand-in approver of the person whose root `root` is:
/// the key kept for them in `dir`, and the certificate their root signs for
/// it.
///
/// A host its person's owner runs holds their root (a deployment's root
/// file), and approves for them where nobody else can: a workspace's browser
/// session joins as a device of theirs, which only a stand-in certifies. A
/// stand-in already live in `dir` is left as it is.
///
/// What keeps one is a host that places browser sessions, which is the agents
/// feature's; the test below keeps one without it.
///
/// # Errors
///
/// The directory cannot be read or written, or the root would not sign it.
#[cfg(any(test, feature = "agents"))]
pub(crate) fn keep(dir: &std::path::Path, root: &SigningKey) -> Result<(), String> {
    let stands = StandIns::new(dir.to_path_buf());
    let user = root.verifying_key().to_bytes();
    let now = tilo_clock::unix_now_ns();
    if stands.live(&user, &RevocationList::new(), now).is_some() {
        return Ok(());
    }
    let key = stands
        .key(&user)
        .map_err(|refusal| refusal.message().to_owned())?;
    let expires = now.saturating_add(
        tilo_relay_protocol::login_code::STAND_IN_LIFETIME_MS
            .saturating_sub(60_000)
            .saturating_mul(1_000_000),
    );
    let mut cert = tilo_user_identity::prepare_stand_in_cert(
        user,
        key.to_bytes(),
        "Browser sessions",
        now,
        expires,
    );
    tilo_user_identity::sign_device_cert(&mut cert, root).map_err(|error| error.to_string())?;
    stands
        .certify(cert, now)
        .map(|_| ())
        .map_err(|refusal| refusal.message().to_owned())
}

/// The stand-in certificate and key this host keeps for `user`, when it
/// keeps a live one.
pub(crate) fn kept(dir: &std::path::Path, user: &[u8; 32]) -> Option<(SigningKey, DeviceCert)> {
    StandIns::new(dir.to_path_buf()).live(user, &RevocationList::new(), tilo_clock::unix_now_ns())
}

impl StandIns {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            kept: Mutex::default(),
        }
    }

    fn key_path(&self, user: &[u8; 32]) -> PathBuf {
        self.dir.join(format!("{}.key", hex::encode(user)))
    }

    fn cert_path(&self, user: &[u8; 32]) -> PathBuf {
        self.dir.join(format!("{}.cert", hex::encode(user)))
    }

    /// `user`'s stand-in, read from the state directory the first time.
    fn load<'a>(
        &self,
        kept: &'a mut HashMap<[u8; 32], Kept>,
        user: &[u8; 32],
    ) -> Option<&'a mut Kept> {
        if !kept.contains_key(user) {
            let seed = std::fs::read(self.key_path(user)).ok()?;
            let seed: [u8; 32] = seed.as_slice().try_into().ok()?;
            let cert = std::fs::read(self.cert_path(user))
                .ok()
                .and_then(|bytes| postcard::from_bytes(&bytes).ok());
            kept.insert(
                *user,
                Kept {
                    key: SigningKey::from_bytes(&seed),
                    cert,
                },
            );
        }
        kept.get_mut(user)
    }

    fn count_on_disk(&self) -> usize {
        std::fs::read_dir(&self.dir).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "key"))
                .count()
        })
    }

    /// Answer a root's request of its stand-in.
    pub(crate) fn answer(
        &self,
        request: StandInRequest,
        now_ns: u64,
    ) -> Result<StandInReply, Refusal> {
        match request {
            StandInRequest::Key {
                user,
                signed_at_ns,
                signature,
            } => {
                let user = root_signed("key", &user, signed_at_ns, &signature, now_ns)?;
                self.key(&user).map(|key| StandInReply::Key {
                    key: hex::encode(key.to_bytes()),
                })
            }
            StandInRequest::Certify { cert } => {
                let bytes = hex::decode(cert)
                    .map_err(|_| Refusal::Malformed("a certificate is postcard as hex"))?;
                let cert: DeviceCert = postcard::from_bytes(&bytes)
                    .map_err(|_| Refusal::Malformed("a certificate is postcard as hex"))?;
                self.certify(cert, now_ns)
                    .map(|expires_at_ms| StandInReply::Kept { expires_at_ms })
            }
            StandInRequest::Retire {
                user,
                signed_at_ns,
                signature,
            } => {
                let user = root_signed("retire", &user, signed_at_ns, &signature, now_ns)?;
                self.retire(&user).map(|()| StandInReply::Retired)
            }
        }
    }

    /// Run `f` on the stand-ins kept, under the lock.
    fn with_kept<R>(&self, f: impl FnOnce(&mut HashMap<[u8; 32], Kept>) -> R) -> R {
        f(&mut self.kept.lock_recover())
    }

    /// `user`'s stand-in key, made the first time.
    fn key(&self, user: &[u8; 32]) -> Result<VerifyingKey, Refusal> {
        self.with_kept(|kept| self.make_key(kept, user))
    }

    fn make_key(
        &self,
        kept: &mut HashMap<[u8; 32], Kept>,
        user: &[u8; 32],
    ) -> Result<VerifyingKey, Refusal> {
        if let Some(kept) = self.load(kept, user) {
            return Ok(kept.key.verifying_key());
        }
        if self.count_on_disk() >= MAX_STAND_INS {
            return Err(Refusal::Full);
        }
        let key = SigningKey::from_bytes(&rand::random());
        std::fs::create_dir_all(&self.dir).map_err(|_| Refusal::Storage)?;
        let seed = zeroize::Zeroizing::new(key.to_bytes());
        tilo_files::native::atomic::write_with_mode(&self.key_path(user), seed.as_slice(), 0o600)
            .map_err(|_| Refusal::Storage)?;
        let public = key.verifying_key();
        kept.insert(*user, Kept { key, cert: None });
        Ok(public)
    }

    /// Keep the root-signed certificate of a stand-in key this host holds.
    fn certify(&self, cert: DeviceCert, now_ns: u64) -> Result<u64, Refusal> {
        check_stand_in_cert(&cert, now_ns)?;
        let user = cert.user_public_key;
        self.with_kept(|kept| {
            let kept = self.load(kept, &user).ok_or(Refusal::Unknown)?;
            self.keep_cert(kept, cert)
        })
    }

    fn keep_cert(&self, kept: &mut Kept, cert: DeviceCert) -> Result<u64, Refusal> {
        let user = cert.user_public_key;
        if kept.key.verifying_key().to_bytes() != cert.device_public_key {
            return Err(Refusal::Forbidden(
                "the certificate is for another key than this person's stand-in",
            ));
        }
        let bytes = postcard::to_allocvec(&cert).map_err(|_| Refusal::Storage)?;
        tilo_files::native::atomic::write_with_mode(&self.cert_path(&user), &bytes, 0o600)
            .map_err(|_| Refusal::Storage)?;
        let expires_at_ms = cert.expires_at_ns.map_or(0, |expires| expires / 1_000_000);
        kept.cert = Some(cert);
        Ok(expires_at_ms)
    }

    /// Forget `user`'s stand-in key and certificate.
    fn retire(&self, user: &[u8; 32]) -> Result<(), Refusal> {
        self.with_kept(|kept| {
            kept.remove(user);
            for path in [self.key_path(user), self.cert_path(user)] {
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(Refusal::Storage),
                }
            }
            Ok(())
        })
    }

    /// `user`'s stand-in, when it may approve at `now_ns`: its key and its
    /// live, unrevoked certificate.
    pub(crate) fn live(
        &self,
        user: &[u8; 32],
        revocations: &RevocationList,
        now_ns: u64,
    ) -> Option<(SigningKey, DeviceCert)> {
        self.with_kept(|kept| {
            let kept = self.load(kept, user)?;
            let cert = kept.cert.as_ref()?;
            if check_stand_in_cert(cert, now_ns).is_err() || revocations.refuses_at(cert, now_ns) {
                return None;
            }
            Some((kept.key.clone(), cert.clone()))
        })
    }
}

/// Check `cert` is a stand-in's certificate valid at `now_ns`: root-signed,
/// carrying the stand-in capability and nothing else, for at most
/// [`tilo_relay_protocol::login_code::STAND_IN_LIFETIME_MS`].
fn check_stand_in_cert(cert: &DeviceCert, now_ns: u64) -> Result<(), Refusal> {
    tilo_user_identity::verify_stand_in_cert_at(cert, now_ns)
        .map_err(|_| Refusal::Forbidden("not a live stand-in certificate of this person"))
}

#[cfg(test)]
mod tests {
    use super::{keep, kept};

    /// The root a host holds signs one stand-in for it, kept across starts.
    #[test]
    fn a_host_holding_the_root_makes_itself_the_persons_stand_in() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let user = root.verifying_key().to_bytes();
        assert!(kept(dir.path(), &user).is_none(), "nothing kept yet");

        keep(dir.path(), &root).unwrap();
        let (key, cert) = kept(dir.path(), &user).expect("the stand-in is kept");
        assert_eq!(cert.user_public_key, user);
        assert_eq!(cert.device_public_key, key.verifying_key().to_bytes());
        assert!(
            cert.capabilities.stand_in,
            "it is an approver, nothing else"
        );
        assert_eq!(cert.issuer, None, "the root signed it");
        tilo_user_identity::verify_stand_in_cert_at(&cert, tilo_clock::unix_now_ns())
            .expect("the certificate is live");

        // A later start keeps the one it has, key and certificate.
        keep(dir.path(), &root).unwrap();
        assert_eq!(kept(dir.path(), &user).unwrap().1, cert);
    }
}
