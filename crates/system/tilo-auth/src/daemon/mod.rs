//! `tilo-authd`: owns every person's record and does every unwrap.
//!
//! Requests are answered one at a time: each may ask the TPM or spend
//! 256 MiB on Argon2id, and neither should run twice at once.

mod enroll;
pub mod server;
pub mod volume;

use crate::accounts::{Account, Accounts, Legacy};
use crate::guard::Guard;
use crate::handoff;
use crate::keyring;
use crate::lockout_store::{self, LockoutStore, Opener};
use crate::luks;
use crate::protocol::{PendingSignIn, Refusal, Reply, Request};
use crate::record::{self, Record, Store};
use crate::recovery::RecoveryKey;
use crate::secret::Secret;
use crate::strength;
use crate::tpm::{self, Tries};
use crate::userkey::UserKey;
use crate::wrapper::{self, Kind, Outcome, Tpm, Wrapper};
use anyhow::{Context as _, Result, bail};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tilo_rollback::exec::Real;
use tilo_rollback::policy::Workspace;
use tilo_seal::envelope::PasswordParams;
use volume::Locate;

/// How long a boot sign-in waits for its session.
pub const BOOT_SIGN_IN_FOR: Duration = Duration::from_secs(handoff::TIMEOUT_S as u64);

pub struct Config {
    pub socket: PathBuf,
    pub records: PathBuf,
    /// The root `/etc/passwd` and `/etc/group` are read under.
    pub accounts_root: PathBuf,
    pub lockout: PathBuf,
    pub lockout_legacy: PathBuf,
    /// The disk passphrase tilo-rollback's unlock left for this boot.
    pub proof_file: PathBuf,
    /// A root-only directory for the tools' scratch files.
    pub scratch: PathBuf,
    /// The account the login screen runs as.
    pub greeter_user: String,
    /// The uid that may do what only root may.
    pub privileged_uid: u32,
    pub tcti: Option<String>,
    pub passphrase_params: PasswordParams,
    pub volume: Locate,
    /// The boot handoff key's description; `None` takes none.
    pub handoff: Option<String>,
    /// The linked volume key's description; `None` uses none.
    pub volume_key: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket: PathBuf::from(crate::SOCKET),
            records: PathBuf::from(record::DIR),
            accounts_root: PathBuf::from("/"),
            lockout: PathBuf::from(lockout_store::PATH),
            lockout_legacy: PathBuf::from(lockout_store::LEGACY_PATH),
            proof_file: PathBuf::from("/run/tilo-rollback/proof.key"),
            scratch: PathBuf::from("/run/tilo-auth/work"),
            greeter_user: "greetd".into(),
            privileged_uid: 0,
            tcti: None,
            passphrase_params: wrapper::PASSPHRASE_PARAMS,
            volume: Locate::Root,
            handoff: Some(handoff::DESCRIPTION.into()),
            volume_key: Some(volume::LINKED_VOLUME_KEY.into()),
        }
    }
}

/// Who is asking, from the socket's peer credentials.
#[derive(Clone, Copy, Debug)]
pub struct Peer {
    pub uid: u32,
}

/// What goes back on the socket.
pub enum Answer {
    Reply(Reply),
    Pending(Option<PendingSignIn>),
}

struct BootSignIn {
    uid: u32,
    name: String,
    /// Held for the personal storage that unlocks with it.
    _key: UserKey,
    at: Instant,
    claimed: bool,
}

struct State {
    guard: Guard,
    boot: Option<BootSignIn>,
}

pub struct Daemon {
    cfg: Config,
    legacy: Box<dyn Legacy>,
    store: Store,
    accounts: Accounts,
    lockout: LockoutStore,
    state: Mutex<State>,
}

/// One request's scratch directory and tool runner.
struct Work {
    _dir: tempfile::TempDir,
    runner: Real,
    ws: Workspace,
}

impl Work {
    fn tpm(&self) -> Tpm<'_> {
        Tpm {
            runner: &self.runner,
            ws: &self.ws,
        }
    }
}

fn tries_reply(refusal: Refusal, tries: Option<Tries>) -> Reply {
    Reply {
        tries_left: tries.map(|t| t.left),
        retry_in_s: tries.and_then(|t| t.retry_in_s),
        ..Reply::refused(refusal)
    }
}

/// What offering a secret to a person's record came to.
enum Checked {
    Key(UserKey),
    Refused(Reply),
}

/// Offer `secret` to an enrolled record. A repeated wrong entry is answered
/// without asking the TPM; passphrase wrappers, which cost no TPM try, go
/// first.
fn check(state: &mut State, work: &Work, record: &Record, secret: &Secret) -> Result<Checked> {
    let uid = record.uid;
    if state.guard.already_wrong(uid, secret, Instant::now()) {
        return Ok(Checked::Refused(tries_reply(
            Refusal::Wrong,
            tpm::tries(&work.runner),
        )));
    }
    let mut ordered: Vec<&Wrapper> = record
        .known()
        .filter(|w| w.kind() == Kind::Passphrase)
        .collect();
    ordered.extend(record.known().filter(|w| w.kind() == Kind::Tpm));
    let mut last = None;
    for wrapper in ordered {
        match wrapper.open_with_secret(Some(work.tpm()), uid, secret)? {
            Some(Outcome::Key(key)) => {
                state.guard.succeeded(uid);
                return Ok(Checked::Key(key));
            }
            Some(Outcome::LockedOut(tries)) => {
                return Ok(Checked::Refused(tries_reply(Refusal::LockedOut, tries)));
            }
            Some(Outcome::Wrong(tries)) => last = Some(tries),
            None => {}
        }
    }
    state.guard.remember_wrong(uid, secret, Instant::now());
    Ok(Checked::Refused(tries_reply(
        Refusal::Wrong,
        last.flatten(),
    )))
}

fn open_with_recovery(record: &Record, typed: &str) -> Result<Option<(UserKey, RecoveryKey)>> {
    let Some(recovery) = RecoveryKey::parse(typed) else {
        return Ok(None);
    };
    for wrapper in record.known() {
        if let Some(key) = wrapper.open_with_recovery(record.uid, &recovery)? {
            return Ok(Some((key, recovery)));
        }
    }
    Ok(None)
}

/// How a new secret is proven to be the person's right to change it.
enum Proof<'a> {
    Current(&'a str),
    Recovery(&'a str),
}

impl Daemon {
    pub fn new(cfg: Config, legacy: Box<dyn Legacy>) -> Result<Self> {
        Ok(Self {
            store: Store::new(&cfg.records),
            accounts: Accounts::new(&cfg.accounts_root),
            lockout: LockoutStore::new(&cfg.lockout, &cfg.lockout_legacy),
            state: Mutex::new(State {
                guard: Guard::new()?,
                boot: None,
            }),
            legacy,
            cfg,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn work(&self) -> Result<Work> {
        tilo_files::native::atomic::create_private_directory(&self.cfg.scratch)
            .with_context(|| format!("creating {}", self.cfg.scratch.display()))?;
        let dir = tempfile::Builder::new()
            .prefix("op.")
            .tempdir_in(&self.cfg.scratch)
            .context("creating a scratch directory")?;
        let runner = Real::new()
            .with_tcti(self.cfg.tcti.as_deref())
            .with_cwd(dir.path());
        let ws = Workspace::new(dir.path());
        Ok(Work {
            _dir: dir,
            runner,
            ws,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Read and destroy the boot handoff, keeping it only for a person whose
    /// keyslot it opens: a stale or foreign one is dropped.
    pub fn take_boot_handoff(&self) {
        let Some(description) = &self.cfg.handoff else {
            return;
        };
        let handoff = match handoff::take(description) {
            Ok(Some(handoff)) => handoff,
            Ok(None) => return,
            Err(e) => {
                eprintln!("tilo-authd: could not read the boot sign-in: {e}");
                return;
            }
        };
        match self.boot_key_is_genuine(handoff.uid, &handoff.key) {
            Ok(Some(name)) => {
                eprintln!("tilo-authd: uid {} signed in at boot", handoff.uid);
                self.lock().boot = Some(BootSignIn {
                    uid: handoff.uid,
                    name,
                    _key: handoff.key,
                    at: Instant::now(),
                    claimed: false,
                });
            }
            Ok(None) => eprintln!("tilo-authd: ignoring a boot sign-in that opens no keyslot"),
            Err(e) => eprintln!("tilo-authd: could not check the boot sign-in: {e:#}"),
        }
    }

    /// The linked volume key is for adding the first person's keyslot; a
    /// volume that already has a sign-in, or none at all, never needs it.
    pub fn settle_volume_key(&self) {
        if self.cfg.volume_key.is_none() {
            return;
        }
        let needed = self.work().and_then(|work| {
            let Some(volume) = self.cfg.volume.find(&work.runner) else {
                return Ok(false);
            };
            Ok(luks::tokens(&work.runner, &volume.device)?.is_empty())
        });
        match needed {
            Ok(true) => {}
            Ok(false) => self.forget_volume_key(),
            Err(e) => {
                eprintln!("tilo-authd: {e:#}");
                self.forget_volume_key();
            }
        }
    }

    fn forget_volume_key(&self) {
        if let Some(description) = &self.cfg.volume_key
            && let Err(e) = keyring::forget(description)
        {
            eprintln!("tilo-authd: could not unlink the linked volume key: {e}");
        }
    }

    fn boot_key_is_genuine(&self, uid: u32, key: &UserKey) -> Result<Option<String>> {
        let Some(record) = self.store.load(uid)? else {
            return Ok(None);
        };
        let Some(slot) = record.system_keyslot else {
            return Ok(None);
        };
        let work = self.work()?;
        let Some(volume) = self.cfg.volume.find(&work.runner) else {
            return Ok(None);
        };
        let passphrase = key.system_volume_passphrase();
        Ok(luks::opens(
            &work.runner,
            &work.ws,
            &volume.device,
            passphrase.as_bytes(),
            slot,
        )?
        .then_some(record.name))
    }

    pub fn handle(&self, peer: Peer, request: Request) -> Answer {
        let result = match request {
            Request::PendingSignIn => return Answer::Pending(self.pending_sign_in(peer)),
            Request::Verify { user, secret } => self.verify(peer, &user, &secret),
            Request::Status { user } => self.status(peer, &user),
            Request::Enroll { user, secret } => self.enroll_request(peer, &user, &secret),
            Request::SetSecret {
                user,
                current,
                recovery,
                new,
            } => match (current, recovery) {
                (Some(current), _) => self.set_secret(peer, &user, &Proof::Current(&current), &new),
                (None, Some(recovery)) => {
                    self.set_secret(peer, &user, &Proof::Recovery(&recovery), &new)
                }
                (None, None) => Ok(Reply::refused(Refusal::BadRequest)),
            },
            Request::AddRecovery {
                user,
                secret,
                recovery,
            } => self.add_recovery(peer, &user, &secret, &recovery),
            Request::ClaimBootSignIn { user } => Ok(self.claim_boot_sign_in(peer, &user)),
        };
        Answer::Reply(result.unwrap_or_else(|e| {
            eprintln!("tilo-authd: {e:#}");
            Reply::refused(Refusal::Unavailable)
        }))
    }

    fn privileged(&self, peer: Peer) -> bool {
        peer.uid == self.cfg.privileged_uid
    }

    /// The account `user` names, when `peer` may ask about it.
    fn subject(&self, peer: Peer, user: &str) -> Result<Result<Account, Reply>> {
        let account = self.accounts.by_name(user)?;
        Ok(match account {
            Some(account) if self.privileged(peer) || peer.uid == account.uid => Ok(account),
            None if self.privileged(peer) => Err(Reply::refused(Refusal::NotEnrolled)),
            _ => Err(Reply::refused(Refusal::NotPermitted)),
        })
    }

    fn pending_sign_in(&self, peer: Peer) -> Option<PendingSignIn> {
        let greeter = self
            .accounts
            .by_name(&self.cfg.greeter_user)
            .ok()
            .flatten()
            .map(|a| a.uid);
        if !self.privileged(peer) && greeter != Some(peer.uid) {
            return None;
        }
        self.lock()
            .boot
            .as_ref()
            .filter(|boot| !boot.claimed && boot.at.elapsed() < BOOT_SIGN_IN_FOR)
            .map(|boot| PendingSignIn {
                name: boot.name.clone(),
            })
    }

    fn claim_boot_sign_in(&self, peer: Peer, user: &str) -> Reply {
        if !self.privileged(peer) {
            return Reply::refused(Refusal::NotPermitted);
        }
        let claimed = self
            .lock()
            .boot
            .as_mut()
            .filter(|boot| {
                !boot.claimed && boot.name == user && boot.at.elapsed() < BOOT_SIGN_IN_FOR
            })
            .map(|boot| {
                boot.claimed = true;
                boot.uid
            });
        match claimed {
            Some(uid) => {
                eprintln!("tilo-authd: uid {uid} started their session from the boot sign-in");
                Reply::success()
            }
            None => Reply::refused(Refusal::NoBootSignIn),
        }
    }

    fn status(&self, peer: Peer, user: &str) -> Result<Reply> {
        let account = match self.subject(peer, user)? {
            Ok(account) => account,
            Err(reply) => return Ok(reply),
        };
        let record = self.store.load(account.uid)?;
        Ok(Reply {
            enrolled: Some(record.as_ref().is_some_and(Record::enrolled)),
            factors: record
                .map(|r| {
                    r.factors()
                        .into_iter()
                        .filter_map(|f| serde_json::to_value(f).ok())
                        .filter_map(|v| v.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
            ..Reply::success()
        })
    }

    fn verify(&self, peer: Peer, user: &str, typed: &str) -> Result<Reply> {
        let account = match self.subject(peer, user)? {
            Ok(account) => account,
            Err(reply) => return Ok(reply),
        };
        let mut state = self.lock();
        self.verify_locked(&mut state, &account, typed)
    }

    /// Whatever it answers, the first sign-in attempt of the boot is the last
    /// that may use the linked volume key.
    fn verify_locked(&self, state: &mut State, account: &Account, typed: &str) -> Result<Reply> {
        let reply = self.verify_once(state, account, typed);
        self.forget_volume_key();
        reply
    }

    fn verify_once(&self, state: &mut State, account: &Account, typed: &str) -> Result<Reply> {
        let secret = Secret::new(typed);
        let work = self.work()?;
        match self.store.load(account.uid)? {
            Some(record) if record.enrolled() => match check(state, &work, &record, &secret)? {
                Checked::Key(key) => {
                    self.after_sign_in(&work, account, record, &key, Some(typed));
                    Ok(Reply::success())
                }
                Checked::Refused(reply) => Ok(reply),
            },
            _ => self.verify_legacy(&work, account, typed, &secret),
        }
    }

    fn enroll_request(&self, peer: Peer, user: &str, typed: &str) -> Result<Reply> {
        let account = match self.subject(peer, user)? {
            Ok(account) => account,
            Err(reply) => return Ok(reply),
        };
        let mut state = self.lock();
        if self.store.load(account.uid)?.is_some_and(|r| r.enrolled()) {
            return Ok(Reply::success());
        }
        self.verify_locked(&mut state, &account, typed)
    }

    fn set_secret(&self, peer: Peer, user: &str, proof: &Proof<'_>, new: &str) -> Result<Reply> {
        let account = match self.subject(peer, user)? {
            Ok(account) => account,
            Err(reply) => return Ok(reply),
        };
        if matches!(proof, Proof::Recovery(_)) && !self.privileged(peer) {
            return Ok(Reply::refused(Refusal::NotPermitted));
        }
        let mut state = self.lock();
        self.set_secret_locked(&mut state, &account, proof, &Secret::new(new))
    }

    fn set_secret_locked(
        &self,
        state: &mut State,
        account: &Account,
        proof: &Proof<'_>,
        new: &Secret,
    ) -> Result<Reply> {
        let work = self.work()?;
        let Some(mut record) = self.store.load(account.uid)?.filter(Record::enrolled) else {
            return Ok(Reply::refused(Refusal::NotEnrolled));
        };
        let tpm_usable = tpm::usable(&work.runner);
        if !tpm_usable && !strength::passphrase_is_strong(new) {
            return Ok(Reply::refused(Refusal::WeakSecret));
        }
        let (key, recovery_key) = match proof {
            Proof::Current(current) => match check(state, &work, &record, &Secret::new(current))? {
                Checked::Key(key) => (key, None),
                Checked::Refused(reply) => return Ok(reply),
            },
            Proof::Recovery(recovery) => match open_with_recovery(&record, recovery)? {
                Some((key, recovery_key)) => (key, Some(recovery_key)),
                None => return Ok(Reply::refused(Refusal::Wrong)),
            },
        };
        // Recovering ends a lockout first: a locked-out TPM seals nothing.
        if let Some(recovery_key) = &recovery_key {
            self.reset_lockout_with_recovery(&work, &record, &key, recovery_key);
        }
        let wrapper = if tpm_usable {
            Wrapper::tpm(work.tpm(), &key, new)?
        } else {
            Wrapper::passphrase(&key, record.uid, new, self.cfg.passphrase_params)?
        };
        if !matches!(
            wrapper.open_with_secret(Some(work.tpm()), record.uid, new)?,
            Some(Outcome::Key(ref opened)) if opened.expose() == key.expose()
        ) {
            bail!(
                "the new wrapper for uid {} does not open; keeping the old one",
                record.uid
            );
        }
        record.set_secret_wrapper(wrapper);
        record.secret = Some(record::SecretKind::of(new));
        self.store.save(&record)?;
        state.guard.succeeded(record.uid);
        self.mirror(&work, &record);
        eprintln!("tilo-authd: uid {} has a new secret", record.uid);
        Ok(Reply::success())
    }

    fn add_recovery(&self, peer: Peer, user: &str, typed: &str, recovery: &str) -> Result<Reply> {
        if !self.privileged(peer) {
            return Ok(Reply::refused(Refusal::NotPermitted));
        }
        let account = match self.subject(peer, user)? {
            Ok(account) => account,
            Err(reply) => return Ok(reply),
        };
        let Some(recovery_key) = RecoveryKey::parse(recovery) else {
            return Ok(Reply::refused(Refusal::BadRequest));
        };
        let mut state = self.lock();
        self.add_recovery_locked(&mut state, &account, &Secret::new(typed), &recovery_key)
    }

    fn add_recovery_locked(
        &self,
        state: &mut State,
        account: &Account,
        secret: &Secret,
        recovery_key: &RecoveryKey,
    ) -> Result<Reply> {
        let work = self.work()?;
        let Some(mut record) = self.store.load(account.uid)?.filter(Record::enrolled) else {
            return Ok(Reply::refused(Refusal::NotEnrolled));
        };
        let key = match check(state, &work, &record, secret)? {
            Checked::Key(key) => key,
            Checked::Refused(reply) => return Ok(reply),
        };
        // The machine's recovery key is the one its recovery keyslot takes.
        if record.unlocks_system
            && let Some(volume) = self.cfg.volume.find(&work.runner)
            && !tilo_rollback::keyslots::verify_passphrase(
                &work.runner,
                &work.ws,
                &volume.device,
                recovery_key.formatted().as_bytes(),
                None,
            )?
        {
            return Ok(Reply::refused(Refusal::Wrong));
        }
        record.set_recovery_wrapper(Wrapper::recovery(&key, record.uid, recovery_key)?);
        self.store.save(&record)?;
        self.mirror(&work, &record);
        if record.unlocks_system {
            self.adopt_lockout(
                &Opener::Recovery(recovery_key),
                Some(&Opener::User {
                    uid: record.uid,
                    key: &key,
                }),
            );
        }
        eprintln!(
            "tilo-authd: uid {} can recover with their recovery key",
            record.uid
        );
        Ok(Reply::success())
    }

    fn adopt_lockout(&self, opener: &Opener<'_>, existing: Option<&Opener<'_>>) {
        match self.lockout.adopt(opener, existing) {
            Ok(lockout_store::Adopted::Complete) => eprintln!(
                "tilo-authd: the TPM lockout authorisation is wrapped and its file is gone"
            ),
            Ok(_) => {}
            Err(e) => eprintln!("tilo-authd: could not wrap the TPM lockout authorisation: {e:#}"),
        }
    }

    /// Recovering ends a lockout: the recovery key opens the authorisation
    /// that resets the TPM's failure counter.
    fn reset_lockout_with_recovery(
        &self,
        work: &Work,
        record: &Record,
        key: &UserKey,
        recovery: &RecoveryKey,
    ) {
        if record.unlocks_system {
            self.adopt_lockout(
                &Opener::Recovery(recovery),
                Some(&Opener::User {
                    uid: record.uid,
                    key,
                }),
            );
        }
        let auth = match self.lockout.open(&Opener::Recovery(recovery)) {
            Ok(Some(auth)) => auth,
            Ok(None) => return,
            Err(e) => {
                eprintln!("tilo-authd: could not open the TPM lockout authorisation: {e:#}");
                return;
            }
        };
        match tilo_rollback::lockout::clear(&work.runner, &auth, &work.ws.dir) {
            Ok(()) => eprintln!("tilo-authd: reset the TPM's failure counter"),
            Err(e) => eprintln!("tilo-authd: {e:#}"),
        }
    }
}
