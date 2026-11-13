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
//! alone, beside its certificate once the root sent it. A tab asks for one
//! only once its person's agent is in (a visitor who does nothing has
//! none), and the host forgets what can approve nothing any more: a key
//! whose root never certified it within [`UNCERTIFIED_MS`], and one whose
//! certificate ran out. New keys are budgeted per client address, never
//! counted in all.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::certify::MEMBER_APPROVAL_SKEW_NS;
use tilo_relay_protocol::login_code::{StandInReply, StandInRequest};
use tilo_user_identity::{DeviceCert, RevocationList};

/// How long a key the root has not certified yet is kept: the tab certifies
/// it right after asking for it.
pub(crate) const UNCERTIFIED_MS: u64 = 10 * 60 * 1000;
/// New stand-in keys one client address may have made in a minute.
pub(crate) const KEYS_PER_ADDRESS: u32 = 10;
/// How often the host looks for stand-ins to forget.
const PRUNE_EVERY_MS: u64 = 60 * 1000;
/// The window the budget counts in.
const WINDOW_MS: u64 = 60 * 1000;

/// Why a stand-in request was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `400`.
    Malformed(&'static str),
    /// `403`.
    Forbidden(&'static str),
    /// `404`: no stand-in key for this person.
    Unknown,
    /// `429`: too many new keys from this address this minute.
    TooMany,
    /// `500`.
    Storage,
}

impl Refusal {
    pub(crate) const fn message(&self) -> &'static str {
        match self {
            Self::Malformed(message) | Self::Forbidden(message) => message,
            Self::Unknown => "this host keeps no stand-in for this person",
            Self::TooMany => "too many new stand-ins from here; wait a minute",
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
    /// New keys per client address in the current minute, and that minute.
    made: Mutex<(u64, HashMap<std::net::IpAddr, u32>)>,
    /// When the host last looked for stand-ins to forget (Unix ms).
    pruned_at_ms: std::sync::atomic::AtomicU64,
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
            made: Mutex::default(),
            pruned_at_ms: std::sync::atomic::AtomicU64::new(0),
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

    /// Forget, at most once a [`PRUNE_EVERY_MS`], every stand-in that can approve nothing: a key
    /// its root never certified within [`UNCERTIFIED_MS`], and one whose certificate ran out.
    fn prune(&self, kept: &mut HashMap<[u8; 32], Kept>, now_ns: u64) {
        use std::sync::atomic::Ordering;
        let millis = now_ns / 1_000_000;
        let looked_ms = self.pruned_at_ms.load(Ordering::Relaxed);
        if millis.saturating_sub(looked_ms) < PRUNE_EVERY_MS
            || self
                .pruned_at_ms
                .compare_exchange(looked_ms, millis, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "key") {
                continue;
            }
            let Some(user) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(hex32)
            else {
                continue;
            };
            let cert: Option<DeviceCert> = std::fs::read(self.cert_path(&user))
                .ok()
                .and_then(|bytes| postcard::from_bytes(&bytes).ok());
            let forget = match cert {
                Some(cert) => check_stand_in_cert(&cert, now_ns).is_err(),
                None => entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|at| u64::try_from(at.as_millis()).ok())
                    .is_some_and(|at_ms| millis.saturating_sub(at_ms) > UNCERTIFIED_MS),
            };
            if forget {
                kept.remove(&user);
                let _ = std::fs::remove_file(self.cert_path(&user));
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// Count a new key for `client` this minute: whether it is within its budget.
    fn within_budget(&self, client: Option<std::net::IpAddr>, now_ns: u64) -> bool {
        let Some(client) = client else { return true };
        let window = now_ns / 1_000_000 / WINDOW_MS;
        let mut made = self.made.lock_recover();
        if made.0 != window {
            *made = (window, HashMap::new());
        }
        let count = made.1.entry(client).or_default();
        let within = *count < KEYS_PER_ADDRESS;
        if within {
            *count = count.saturating_add(1);
        }
        drop(made);
        within
    }

    /// Answer a root's request of its stand-in.
    pub(crate) fn answer(
        &self,
        request: StandInRequest,
        client: Option<std::net::IpAddr>,
        now_ns: u64,
    ) -> Result<StandInReply, Refusal> {
        match request {
            StandInRequest::Key {
                user,
                signed_at_ns,
                signature,
            } => {
                let user = root_signed("key", &user, signed_at_ns, &signature, now_ns)?;
                self.key_for(&user, client, now_ns)
                    .map(|key| StandInReply::Key {
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
    #[cfg(any(test, feature = "agents"))]
    fn key(&self, user: &[u8; 32]) -> Result<VerifyingKey, Refusal> {
        self.key_for(user, None, tilo_clock::unix_now_ns())
    }

    /// `user`'s stand-in key, made the first time for a request from `client`.
    fn key_for(
        &self,
        user: &[u8; 32],
        client: Option<std::net::IpAddr>,
        now_ns: u64,
    ) -> Result<VerifyingKey, Refusal> {
        self.with_kept(|kept| {
            self.prune(kept, now_ns);
            self.make_key(kept, user, client, now_ns)
        })
    }

    fn make_key(
        &self,
        kept: &mut HashMap<[u8; 32], Kept>,
        user: &[u8; 32],
        client: Option<std::net::IpAddr>,
        now_ns: u64,
    ) -> Result<VerifyingKey, Refusal> {
        if let Some(kept) = self.load(kept, user) {
            return Ok(kept.key.verifying_key());
        }
        if !self.within_budget(client, now_ns) {
            return Err(Refusal::TooMany);
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
    use super::{StandIns, UNCERTIFIED_MS, keep, kept};
    use std::collections::HashMap;

    /// The host forgets a key nobody certified once it is old, and a stand-in whose certificate
    /// ran out; a live one stays. Nothing caps how many people have one.
    #[test]
    fn stand_ins_that_can_approve_nothing_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let user = root.verifying_key().to_bytes();
        keep(dir.path(), &root).unwrap();
        let stands = StandIns::new(dir.path().to_path_buf());
        // Thousands of people who asked for a key and never certified it.
        let abandoned: Vec<[u8; 32]> = (0u16..2_000)
            .map(|index| {
                let mut person = [0; 32];
                person[..2].copy_from_slice(&index.to_le_bytes());
                person[31] = 1;
                stands.key(&person).unwrap();
                person
            })
            .collect();
        let old = std::time::SystemTime::now()
            - std::time::Duration::from_millis(UNCERTIFIED_MS + 60_000);
        for person in &abandoned[..1_500] {
            std::fs::File::options()
                .write(true)
                .open(stands.key_path(person))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        let now = tilo_clock::unix_now_ns();
        // Asking for those keys looked already; this look is a later one.
        stands
            .pruned_at_ms
            .store(0, std::sync::atomic::Ordering::Relaxed);
        stands.prune(&mut HashMap::new(), now);
        assert!(
            !stands.key_path(&abandoned[0]).exists(),
            "an old key nobody certified goes"
        );
        assert!(
            stands.key_path(&abandoned[1_999]).exists(),
            "a fresh one waits for its certificate"
        );
        assert!(
            kept(dir.path(), &user).is_some(),
            "a certified stand-in stays"
        );
        // Its certificate run out, it goes too (looked at no sooner than a minute later).
        let after = now
            + tilo_relay_protocol::login_code::STAND_IN_LIFETIME_MS * 1_000_000
            + 86_400_000_000_000;
        stands.prune(&mut HashMap::new(), after);
        assert!(!stands.key_path(&user).exists() && !stands.cert_path(&user).exists());
    }

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
