//! Two journeys against an emulated TPM and a LUKS2 image on a plain file, driving the production
//! `confirm`/`policy`/`keyslots`/`counter`/`approval` code paths without root, loop devices or a
//! VM: a disk through a run of updates, and the TPM's floor and its authority. Mounting and
//! `tilo-rollback unlock` on a booted machine are not covered.
//!
//! Requires `swtpm`, `tpm2-tools` and `cryptsetup`; each journey gets its own `swtpm`, since both
//! drive dictionary-attack lockout, which a shared instance would leak.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; the arithmetic \
              here crafts an adversarial (stale) counter value for a rollback \
              test, not real user data; a journey is one function, step after step"
)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use tilo_rollback::{
    Config, Pbkdf, approval, confirm, counter, exec, keyslots, lockout, policy,
    policy::{Prediction, Workspace},
    token::{RollbackToken, SealedObject, SignedPrediction},
    tpm_policy,
};
use tilo_test_env::Capability;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_test_env::swtpm::Swtpm;

// Harness

struct Fixture {
    _tpm: Swtpm,
    scratch: tempfile::TempDir,
    runner: exec::Real,
    cfg: Config,
    /// Private half of the stand-in prediction key.
    sign_priv: PathBuf,
    key_name: Vec<u8>,
    /// The floor authority's key Name and the NV index authPolicy it implies.
    authority_name: Vec<u8>,
    nv_auth_policy: tpm_policy::Digest256,
    /// Counter base from the last `floor()` call, used to convert counter values into floor generations.
    base: std::cell::Cell<i64>,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let tpm = match Swtpm::start() {
            Ok(tpm) => tpm,
            Err(why) => {
                tilo_test_env::capability_unavailable(Capability::Swtpm, format_args!("{why}"));
                return None;
            }
        };
        let scratch = tempfile::tempdir().expect("tempdir");
        let runner = exec::Real::new()
            .with_tcti(Some(&tpm.tcti()))
            .with_cwd(scratch.path());
        // Stand-in for keys/tpm2-pcr-signing.*.pem — the real private key is git-excluded.
        let sign_priv = scratch.path().join("sign.priv.pem");
        let sign_pub = scratch.path().join("sign.pub.pem");
        gen_rsa(&sign_priv, &sign_pub);

        // One key serves as both prediction and floor authority (the shipped default); tests
        // needing the distinction use `evil_priv()` as the wrong key.
        let mut cfg = Config {
            pcr_public_key: sign_pub.display().to_string(),
            floor_authority_key: sign_pub.display().to_string(),
            approval_dir: scratch.path().join("approvals").display().to_string(),
            pbkdf: Some(Pbkdf {
                algorithm: "pbkdf2".into(),
                iterations: 1000,
            }),
            ..Config::default()
        };
        cfg.nv_index = counter::DEFAULT_NV_INDEX;

        let ws = Workspace::new(scratch.path());
        let key_name = policy::key_name(&runner, &ws, &sign_pub).expect("key name");
        let authority_name = key_name.clone();
        let nv_auth_policy = counter::auth_policy(&authority_name);
        Some(Fixture {
            _tpm: tpm,
            scratch,
            runner,
            cfg,
            sign_priv,
            key_name,
            authority_name,
            nv_auth_policy,
            base: std::cell::Cell::new(0),
        })
    }

    fn ws(&self) -> Workspace {
        Workspace::new(self.scratch.path())
    }
    fn dir(&self) -> &Path {
        self.scratch.path()
    }
    fn selection(&self) -> String {
        policy::pcr_selection(&self.cfg.pcr_bank, &self.cfg.pcrs)
    }

    /// Private half of a second, unauthorised key, generated for the test that asks: two do,
    /// and generating it in `new` cost every other test a key it never read.
    fn evil_priv(&self) -> PathBuf {
        let evil_priv = self.dir().join("evil.priv.pem");
        let evil_pub = self.dir().join("evil.pub.pem");
        gen_rsa(&evil_priv, &evil_pub);
        evil_priv
    }

    /// Sign a prediction over the live measurements for each of `generations` and write it to
    /// `dir`, as an image ships them. Returns them in the order asked for: a test that also
    /// enrolls one of those generations hands over the prediction it shipped, which is
    /// byte-for-byte what a second `predict` returns (same live PCRs, and an RSASSA-PKCS1-v1_5
    /// signature has no randomness), without a second trial session.
    fn ship_predictions(&self, dir: &Path, generations: &[u64]) -> Vec<Prediction> {
        generations
            .iter()
            .map(|&generation| {
                let p = self.predict(generation, &self.sign_priv, "");
                policy::write_signed_prediction(
                    dir,
                    generation,
                    &self.cfg.pcr_bank,
                    &self.cfg.pcrs,
                    &p,
                )
                .expect("write prediction");
                p
            })
            .collect()
    }

    fn floor(&self, initial_floor: u64) -> counter::FloorState {
        let ws = self.ws();
        counter::define(&self.runner, self.cfg.nv_index, &self.nv_auth_policy, &ws)
            .expect("define");
        let f = counter::materialise(
            &self.runner,
            &ws,
            self.cfg.nv_index,
            self.dir(),
            initial_floor,
            &self.authority_name,
        )
        .expect("materialise");
        self.base.set(f.base);
        f
    }

    fn nv_name(&self) -> Vec<u8> {
        counter::read_name(&self.runner, self.cfg.nv_index).expect("nv name")
    }

    fn approval_dir(&self) -> PathBuf {
        PathBuf::from(&self.cfg.approval_dir)
    }

    /// Issue approvals covering floor generations up to `up_to_floor`. `strict` applies the
    /// safe bound (never above the newest enrolled generation); counter-only tests pass `false`
    /// to move the floor as setup, the same override the CLI checks behind an explicit flag.
    fn issue_approvals(
        &self,
        floor: counter::FloorState,
        enrolled: &[u64],
        up_to_floor: u64,
        strict: bool,
    ) -> Vec<approval::Approval> {
        let ws = self.ws();
        let req = approval::build_request(
            self.cfg.nv_index,
            &self.nv_name(),
            floor,
            enrolled,
            self.cfg.retained_generations,
        )
        .expect("build request");
        let a = approval::issue(
            &self.runner,
            &ws,
            &req,
            &self.sign_priv,
            up_to_floor,
            !strict,
        )
        .expect("issue approvals");
        approval::store(&self.approval_dir(), &a).expect("store approvals");
        a
    }

    fn bump_floor(&self, times: u64) {
        let ws = self.ws();
        let counter_value = self.counter_now();
        let base = self.current_base();
        let floor = counter::FloorState {
            counter: counter_value,
            base,
        };
        let target = floor.floor_generation().saturating_add(times);
        let approvals = self.issue_approvals(floor, &[], target, false);
        let out = counter::advance(
            &self.runner,
            &ws,
            self.cfg.nv_index,
            self.dir(),
            floor,
            target,
            &counter::FloorApprovals {
                authority_public_key: Path::new(&self.cfg.pcr_public_key),
                authority_key_name: &self.authority_name,
                approvals: &approvals,
                dir: Some(&self.approval_dir()),
            },
        )
        .expect("advance the floor");
        assert!(
            out.unapproved.is_empty(),
            "test setup could not advance the floor: {:?}",
            out.unapproved
        );
        assert_eq!(out.steps, times, "expected {times} steps");
    }

    fn approve_up_to(&self, base: i64, generation: u64, target_floor: u64) {
        let floor = counter::FloorState {
            counter: self.counter_now(),
            base,
        };
        self.issue_approvals(floor, &[generation], target_floor, true);
    }

    fn current_base(&self) -> i64 {
        self.base.get()
    }

    fn predict(&self, generation: u64, key: &Path, label: &str) -> Prediction {
        let ws = self.ws();
        let approved =
            policy::approved_policy(&self.runner, &ws, &self.selection(), None).expect("approved");
        let sig =
            policy::sign_prediction(&self.runner, &ws, key, &approved, generation).expect("sign");
        Prediction {
            approved_policy: approved,
            signature: sig,
            label: label.into(),
        }
    }

    fn seal_token(
        &self,
        generation: u64,
        floor: counter::FloorState,
        preds: &[Prediction],
        secret: &[u8],
        pin: Option<&[u8]>,
    ) -> RollbackToken {
        let ws = self.ws();
        let sealed_policy = policy::GenerationPolicy {
            generation,
            key_name: &self.key_name,
            pcr_bank: &self.cfg.pcr_bank,
            pcrs: &self.cfg.pcrs,
            platform_pcrs: &self.cfg.platform_pcrs,
            nv: policy::NvConstraint {
                index: self.cfg.nv_index,
                operand: floor.operand_for(generation).expect("operand"),
            },
            pin: pin.is_some(),
        };
        let digest = policy::seal_policy(&self.runner, &ws, &sealed_policy).expect("seal policy");
        let (pu, pr) = keyslots::seal(&self.runner, &ws, secret, &digest, pin).expect("seal");
        RollbackToken::new(
            0,
            &sealed_policy,
            floor.base,
            &SealedObject {
                public: &pu,
                private: &pr,
                policy_digest: &digest,
            },
            preds,
            None,
        )
    }

    fn try_unseal(
        &self,
        token: &RollbackToken,
        pin: Option<&[u8]>,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let ws = self.ws();
        keyslots::unseal(
            &self.runner,
            &ws,
            token,
            Path::new(&self.cfg.pcr_public_key),
            pin,
        )
        .map(|o| o.map(|s| s.to_vec()))
    }

    fn counter_now(&self) -> u64 {
        counter::read(&self.runner, self.cfg.nv_index, self.dir())
            .expect("read")
            .expect("materialised")
    }

    fn raw(&self, program: &str, args: &[&str]) -> exec::Output {
        let osargs: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
        exec::Runner::run(&self.runner, program, &osargs, None).expect("running a tool")
    }
}

fn gen_rsa(priv_path: &Path, pub_path: &Path) {
    let generated = tilo_crypto::generate_tpm_authority_key().expect("generate TPM authority key");
    std::fs::write(priv_path, generated.private_key_pem().as_bytes()).expect("write private key");
    std::fs::write(pub_path, generated.public_key_pem().as_bytes()).expect("write public key");
}

fn luks_image(dir: &Path, recovery: &[u8]) -> PathBuf {
    let img = dir.join("disk.img");
    let f = std::fs::File::create(&img).expect("create image");
    f.set_len(48 << 20).expect("size image");
    drop(f);
    let rec = dir.join("recovery.key");
    std::fs::write(&rec, recovery).expect("write recovery key");
    let ok = Command::new("cryptsetup")
        .args([
            "luksFormat",
            "--type",
            "luks2",
            "--batch-mode",
            "--pbkdf",
            "pbkdf2",
            "--pbkdf-force-iterations",
            "1000",
            "--key-file",
        ])
        .arg(&rec)
        .arg(&img)
        .status_locked()
        .expect("cryptsetup luksFormat")
        .success();
    assert!(ok, "luksFormat failed");
    let _ = std::fs::remove_file(&rec);
    img
}

macro_rules! fixture {
    () => {
        match Fixture::new() {
            Some(f) => f,
            None => return,
        }
    };
}

/// A LUKS2 disk as a machine has it: its recovery key, the predictions its images shipped and
/// the rollback state confirmation keeps.
struct Disk {
    img: PathBuf,
    recovery: &'static [u8],
    preds: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn disk(&self, recovery: &'static [u8]) -> Disk {
        Disk {
            img: luks_image(self.dir(), recovery),
            recovery,
            preds: self.dir().join("predictions"),
            state: self.dir().join("state.json"),
        }
    }

    fn enroll(
        &self,
        disk: &Disk,
        generation: u64,
        floor: counter::FloorState,
        prediction: &Prediction,
        pin: Option<&[u8]>,
    ) -> (u32, u32) {
        keyslots::enroll_generation(
            &self.runner,
            &self.ws(),
            &disk.img,
            &self.cfg,
            &keyslots::Enrollment {
                generation,
                floor,
                predictions: std::slice::from_ref(prediction),
                pin,
                anchors: None,
            },
            disk.recovery,
        )
        .unwrap_or_else(|e| panic!("enroll generation {generation}: {e:#}"))
    }

    /// The token id and token for `generation` in `img`'s header.
    fn token(&self, img: &Path, generation: u64) -> (u32, RollbackToken) {
        keyslots::list_tokens(&self.runner, img)
            .expect("list tokens")
            .into_iter()
            .find(|(_, t)| t.generation == generation)
            .unwrap_or_else(|| panic!("no token for generation {generation}"))
    }

    fn generations(&self, img: &Path) -> Vec<u64> {
        keyslots::list_tokens(&self.runner, img)
            .expect("list tokens")
            .iter()
            .map(|(_, t)| t.generation)
            .collect()
    }

    /// What the CLI's `confirm` asks for after a person booted `generation` with the recovery key
    /// at hand; journeys override the fields their step differs in.
    fn confirm_request<'a>(&'a self, disk: &'a Disk, generation: u64) -> confirm::Request<'a> {
        confirm::Request {
            cfg: &self.cfg,
            device: &disk.img,
            generation,
            prediction_dir: &disk.preds,
            recovery: keyslots::RecoveryCheck::Key(disk.recovery),
            pin: None,
            proof: None,
            floor_at_least: None,
            state_path: &disk.state,
            scratch: self.dir(),
            dry_run: false,
        }
    }

    fn confirm(&self, request: &confirm::Request<'_>) -> anyhow::Result<confirm::Outcome> {
        confirm::run(&self.runner, &self.ws(), request)
    }

    fn recovery_opens(&self, disk: &Disk) -> bool {
        keyslots::verify_recovery(&self.runner, &self.ws(), &disk.img, disk.recovery)
            .expect("verify recovery")
    }

    fn opens(&self, img: &Path, secret: &[u8], slot: Option<u32>) -> bool {
        keyslots::verify_passphrase(&self.runner, &self.ws(), img, secret, slot).expect("verify")
    }
}

fn luks_header(f: &Fixture, action: &str, img: &Path, backup: &Path) {
    let mut args = vec![action, "--header-backup-file", backup.to_str().unwrap()];
    if action == "luksHeaderRestore" {
        args.push("--batch-mode");
    }
    args.push(img.to_str().unwrap());
    assert!(
        f.raw("cryptsetup", &args).ok(),
        "cryptsetup {action} failed"
    );
}

/// A machine's disk through a run of updates, driven through `confirm::run`, the function the
/// CLI's `confirm` calls, so the order under test is the order the machine uses: enrollment,
/// a staged update, a dry run, confirmations that prune the keyslot window and raise the floor
/// by the authority's approvals, an unreachable authority, the header-restore attack, a PIN and
/// the unattended confirmation after a reboot, a declared floor for a security fix, and the
/// independent unlock path vanishing. The recovery key opens the disk at every step.
#[test]
fn a_disk_confirms_its_generations_through_updates_and_the_floor_refuses_every_rollback() {
    let f = fixture!();
    let disk = f.disk(b"recovery-key-for-the-disk-journey");
    let ws = f.ws();
    let shipped = f.ship_predictions(&disk.preds, &[5, 6, 7, 8, 9, 10]);
    let prediction = |generation: u64| &shipped[usize::try_from(generation - 5).unwrap()];
    assert!(
        f.recovery_opens(&disk),
        "the recovery key must open the freshly formatted image"
    );

    // Enrollment: generation 5's keyslot round trips and leaves the recovery key's slot alone.
    let floor = f.floor(3);
    let (slot5, tok5) = f.enroll(&disk, 5, floor, prediction(5), None);
    assert_ne!(slot5, 0, "the recovery key's slot must not be reused");
    let tokens = keyslots::list_tokens(&f.runner, &disk.img).expect("list tokens");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].0, tok5);
    assert_eq!(tokens[0].1.generation, 5);
    let secret5 = f
        .try_unseal(&tokens[0].1, None)
        .expect("unseal 5")
        .expect("a prediction should apply");
    assert!(
        f.opens(&disk.img, &secret5, Some(slot5)),
        "the unsealed secret must open the keyslot the token points at"
    );
    assert!(
        f.recovery_opens(&disk),
        "the recovery key must still work after enrollment"
    );

    // An update stages generation 6 while 5 runs.
    let (slot6, _) = f.enroll(&disk, 6, floor, prediction(6), None);
    let foreign = keyslots::foreign_keyslots(&f.runner, &disk.img).expect("foreign slots");
    assert_eq!(
        foreign,
        vec![0],
        "only the recovery key's slot should be foreign"
    );
    confirm::save_state(
        &disk.state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(5),
            foreign_keyslots: foreign.clone(),
        },
    )
    .expect("seed state");

    // Confirming the running generation keeps the staged one. Treating any generation newer than
    // the running one as stale residue deleted N+1's keyslot, so rebooting into N+1 had nothing
    // to unseal and fell back to the recovery key.
    let o5 = f.confirm(&f.confirm_request(&disk, 5)).expect("confirm 5");
    assert!(
        o5.dropped.is_empty(),
        "confirming 5 must not drop anything, least of all the staged 6: {:?}",
        o5.dropped
    );
    assert!(
        f.generations(&disk.img).contains(&6),
        "generation 6's token must survive"
    );
    assert!(
        keyslots::keyslots_in_use(&f.runner, &disk.img)
            .expect("slots")
            .contains(&slot6),
        "generation 6's keyslot must survive"
    );
    assert!(
        f.try_unseal(&f.token(&disk.img, 6).1, None)
            .expect("unseal 6")
            .is_some(),
        "the staged generation's secret must still be releasable"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&disk.state).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the confirmed state is private");
    }

    // A dry run of the next generation reports and moves nothing.
    let counter_before = f.counter_now();
    let dry = f
        .confirm(&confirm::Request {
            dry_run: true,
            ..f.confirm_request(&disk, 7)
        })
        .expect("dry run");
    assert_eq!(dry.floor.floor_generation(), 3);
    assert_eq!(
        f.counter_now(),
        counter_before,
        "a dry run must not touch the counter"
    );
    assert!(dry.enrolled.is_none());
    assert!(dry.dropped.is_empty());
    assert!(!f.generations(&disk.img).contains(&7));

    // The attacker snapshots the header while generation 5 is still valid.
    let header = f.dir().join("header.bin");
    luks_header(&f, "luksHeaderBackup", &disk.img, &header);

    // Confirming 7 enrolls it; the 3-generation window still holds 5, 6, 7, so nothing is pruned
    // and the floor rises to 5, spending the approvals the authority issued for it.
    f.approve_up_to(floor.base, 7, 5);
    let o7 = f.confirm(&f.confirm_request(&disk, 7)).expect("confirm 7");
    assert!(
        o7.enrolled.is_some(),
        "generation 7 should have been enrolled"
    );
    assert_eq!(o7.floor.floor_generation(), 5);
    assert!(o7.dropped.is_empty(), "5, 6 and 7 all fit the window");

    // No slot is removed without a proven alternative: not on a passphrase that does not open
    // the device, and not on its own secret.
    let e = keyslots::kill_generation_slot(
        &f.runner,
        &ws,
        &disk.img,
        tok5,
        slot5,
        b"not-the-right-passphrase",
    )
    .expect_err("a bogus proof must be refused");
    assert!(
        format!("{e:#}").contains("does not open"),
        "the refusal should say the proof does not open the device: {e:#}"
    );
    let e = keyslots::kill_generation_slot(&f.runner, &ws, &disk.img, tok5, slot5, &secret5)
        .expect_err("self-authorised removal must be refused");
    assert!(
        format!("{e:#}").contains("that very slot"),
        "the refusal should name the self-authorisation: {e:#}"
    );

    // The floor authority is unreachable: confirming 8 still prunes the window and the disk still
    // unlocks; the floor stays where it was and says why.
    assert!(
        approval::load_all(&f.approval_dir())
            .expect("load")
            .is_empty(),
        "spent approvals must be gone, so this step has none"
    );
    let o8 = f
        .confirm(&f.confirm_request(&disk, 8))
        .expect("confirmation must succeed even with no approvals");
    assert_eq!(
        o8.floor.floor_generation(),
        5,
        "the floor must not have moved without an approval"
    );
    assert!(
        !o8.unapproved_steps.is_empty(),
        "the unapproved steps must be reported rather than passed over silently"
    );
    assert_eq!(
        o8.dropped,
        vec![5],
        "the keyslot window must still be pruned"
    );
    assert!(
        !f.opens(&disk.img, &secret5, None),
        "the removed slot must no longer open the device"
    );
    assert!(
        f.try_unseal(&f.token(&disk.img, 8).1, None)
            .expect("unseal 8")
            .is_some(),
        "an unavailable floor authority must never stop the disk unlocking"
    );
    assert!(
        f.recovery_opens(&disk),
        "the recovery key must survive a prune"
    );

    // Once approvals arrive, the same confirmation advances.
    let floor_now = counter::FloorState {
        counter: f.counter_now(),
        base: floor.base,
    };
    f.issue_approvals(floor_now, &f.generations(&disk.img), 6, true);
    let o8 = f
        .confirm(&f.confirm_request(&disk, 8))
        .expect("confirmation with approvals");
    assert_eq!(o8.floor.floor_generation(), 6);
    assert!(o8.unapproved_steps.is_empty(), "{:?}", o8.unapproved_steps);
    assert_eq!(f.generations(&disk.img), vec![8, 7, 6]);
    assert!(
        f.try_unseal(&f.token(&disk.img, 6).1, None)
            .expect("unseal 6")
            .is_some(),
        "the generation at the floor must still unseal"
    );

    // The header-restore attack, on a copy of the disk: restoring the old header resurrects
    // generation 5's token and keyslot, so the keyslot mechanism alone is defeated, but the TPM
    // won't release that passphrase, because the floor lives in the TPM.
    let attacked = f.dir().join("attacked.img");
    std::fs::copy(&disk.img, &attacked).expect("copy the disk");
    luks_header(&f, "luksHeaderRestore", &attacked, &header);
    assert!(
        f.generations(&attacked).contains(&5),
        "the header restore should have brought generation 5's token back — if it did not, this \
         test is not exercising the attack it claims to"
    );
    assert!(
        f.opens(&attacked, &secret5, None),
        "the resurrected keyslot should accept its old passphrase"
    );
    let (_, resurrected) = f.token(&attacked, 5);
    let e = f
        .try_unseal(&resurrected, None)
        .expect_err("the floor must refuse the resurrected generation");
    assert!(
        format!("{e:#}").contains("rollback floor"),
        "the refusal should come from the floor: {e:#}"
    );
    // The header is plaintext, so the token can also be rewritten to claim a bound the floor has
    // not reached. It still reads as consistent, and the session it describes passes the floor
    // comparison, but that is not the policy the secret was sealed to: the bound that counts is
    // in the sealed object, and the TPM keeps the secret.
    let mut rewritten = resurrected;
    rewritten.nv_base += 10;
    rewritten.nv_operand += 10;
    rewritten
        .validate()
        .expect("the rewritten token must be self-consistent, or the TPM is not what refuses it");
    let e = f
        .try_unseal(&rewritten, None)
        .expect_err("a rewritten bound must not release generation 5's secret");
    let msg = format!("{e:#}");
    assert!(
        msg.contains("unsealing generation 5 failed") && !msg.contains("rollback floor"),
        "the TPM's own policy check should refuse, after the rewritten comparison passed: {msg}"
    );

    // Generation 9 is staged sealed with a PIN, the way a real machine is. The PIN is checked
    // before the TPM is asked: a missing one spends none of the lockout's tries, a wrong one does.
    let pin: &[u8] = b"482913";
    f.enroll(&disk, 9, floor, prediction(9), Some(pin));
    let (_, t9) = f.token(&disk.img, 9);
    let boot_secret = f
        .try_unseal(&t9, Some(pin))
        .expect("unseal 9 with the PIN")
        .expect("a prediction applies");
    let untouched = lockout::read(&f.runner).expect("lockout before any refusal");
    let e = f
        .try_unseal(&t9, None)
        .expect_err("a PIN-sealed token must not unseal without a PIN");
    assert!(
        format!("{e:#}").contains("sealed with a PIN but none was supplied"),
        "the refusal should name the missing PIN: {e:#}"
    );
    let before = lockout::read(&f.runner).expect("lockout before");
    assert_eq!(
        before.counter, untouched.counter,
        "a refusal for want of a PIN must not count as an attempt"
    );
    let e = f
        .try_unseal(&t9, Some(b"9999"))
        .expect_err("a wrong PIN must fail");
    assert!(
        format!("{e:#}").contains("PIN"),
        "the failure should mention the PIN: {e:#}"
    );
    let after = lockout::read(&f.runner).expect("lockout after");
    assert!(
        after.counter > before.counter || after.in_lockout,
        "a wrong PIN must count against the dictionary-attack lockout (counter {} -> {}, \
         inLockout {}). If it does not, the sealed object is DA-exempt and a four-digit PIN is \
         four digits of nothing.",
        before.counter,
        after.counter,
        after.in_lockout
    );
    // The `pin` flag is plaintext in the header as well. Cleared, it makes the unlock path build
    // a session without the PIN step, and that is not the policy the secret was sealed to.
    let mut flag_cleared = t9.clone();
    flag_cleared.pin = false;
    let e = f
        .try_unseal(&flag_cleared, None)
        .expect_err("clearing the PIN flag must not release a PIN-sealed secret");
    assert!(
        format!("{e:#}").contains("unsealing generation 9 failed"),
        "the TPM should refuse the session built without the PIN: {e:#}"
    );

    // After the reboot into 9 nothing knows the recovery key or the PIN: confirmation works from
    // the secret boot unlocked with, plus the independent keyslots still being present. Another
    // generation's secret is no proof: otherwise booting an older image could confirm a newer one
    // and drag the floor past what the machine can actually run.
    let secret8 = f
        .try_unseal(&f.token(&disk.img, 8).1, None)
        .expect("unseal 8")
        .expect("applies");
    let before = f.counter_now();
    let e = f
        .confirm(&confirm::Request {
            recovery: keyslots::RecoveryCheck::ForeignSlotsPresent { expected: &foreign },
            proof: Some(&secret8),
            ..f.confirm_request(&disk, 9)
        })
        .expect_err("the wrong generation's secret must not authorise a confirmation");
    assert!(
        format!("{e:#}").contains("does not open generation 9's keyslot"),
        "{e:#}"
    );
    assert_eq!(f.counter_now(), before, "the counter must not have moved");
    f.approve_up_to(floor.base, 9, 7);
    let o9 = f
        .confirm(&confirm::Request {
            recovery: keyslots::RecoveryCheck::ForeignSlotsPresent { expected: &foreign },
            proof: Some(&boot_secret),
            ..f.confirm_request(&disk, 9)
        })
        .expect("unattended confirmation");
    assert_eq!(o9.floor.floor_generation(), 7);
    assert_eq!(o9.dropped, vec![6]);
    assert!(f.recovery_opens(&disk));

    // A declared floor above the running generation is refused before anything moves: an NV
    // increment cannot be undone.
    let before = f.counter_now();
    let e = f
        .confirm(&confirm::Request {
            floor_at_least: Some(10),
            ..f.confirm_request(&disk, 9)
        })
        .expect_err("a floor above the running generation must be refused");
    assert!(
        format!("{e:#}").contains("above the generation being confirmed"),
        "{e:#}"
    );
    assert_eq!(
        f.counter_now(),
        before,
        "a refused confirmation must not have moved the counter — an NV increment cannot be undone"
    );

    // Generation 10 is a security fix and 9 vulnerable: declaring floor 10 closes the window
    // immediately rather than leaving 9 reachable, a deliberate jump the authority approves.
    // 9's keyslot stays in the header (inside the retention window); the floor is what refuses
    // it, so presence is not permission.
    f.approve_up_to(floor.base, 10, 10);
    let o10 = f
        .confirm(&confirm::Request {
            floor_at_least: Some(10),
            ..f.confirm_request(&disk, 10)
        })
        .expect("confirm 10 with a declared floor");
    assert!(o10.enrolled.is_some());
    assert_eq!(
        o10.plan.floor.window_floor, 8,
        "the window alone would have allowed 8"
    );
    assert_eq!(o10.floor.floor_generation(), 10);
    assert_eq!(f.generations(&disk.img), vec![10, 9, 8]);
    let e = f
        .try_unseal(&t9, Some(pin))
        .expect_err("the declared floor must refuse generation 9");
    assert!(format!("{e:#}").contains("rollback floor"), "{e:#}");
    assert!(f.recovery_opens(&disk));

    // The recovery key's slot vanishes: an unattended confirmation notices the independent
    // unlock path is gone and stops before the counter moves.
    let secret10 = f
        .try_unseal(&f.token(&disk.img, 10).1, None)
        .expect("unseal 10")
        .expect("applies");
    let key_file = f.dir().join("kill.key");
    std::fs::write(&key_file, &secret10).expect("write key file");
    assert!(
        f.raw(
            "cryptsetup",
            &[
                "luksKillSlot",
                "--batch-mode",
                "--key-file",
                key_file.to_str().unwrap(),
                disk.img.to_str().unwrap(),
                "0",
            ],
        )
        .ok(),
        "could not remove slot 0, so this step proves nothing"
    );
    let before = f.counter_now();
    let e = f
        .confirm(&confirm::Request {
            recovery: keyslots::RecoveryCheck::ForeignSlotsPresent { expected: &foreign },
            proof: Some(&secret10),
            ..f.confirm_request(&disk, 10)
        })
        .expect_err("a missing independent keyslot must stop the confirmation");
    let msg = format!("{e:#}");
    assert!(
        msg.contains("no longer in") || msg.contains("no keyslot outside"),
        "{msg}"
    );
    assert_eq!(f.counter_now(), before, "the counter must not have moved");
}

/// What a policy is called, the TPM steps that build it in a trial session, and the digest this
/// crate computes for it.
type DigestCase<'a> = (&'a str, Vec<Vec<String>>, Vec<u8>);

/// A trial session is the TPM's own answer for the policy `steps` given.
fn trial_digest(f: &Fixture, steps: &[Vec<String>]) -> Vec<u8> {
    let sess = f.dir().join("purity.ctx");
    let _ = f.raw("tpm2_flushcontext", &["-s"]);
    assert!(
        f.raw("tpm2_startauthsession", &["-S", sess.to_str().unwrap()])
            .ok(),
        "could not start a trial session"
    );
    for step in steps {
        let argv: Vec<&str> = step.iter().map(String::as_str).collect();
        let out = f.raw(argv[0], &argv[1..]);
        assert!(out.ok(), "{argv:?} failed: {}", out.last_error_line());
    }
    let out_file = f.dir().join("purity.digest");
    assert!(
        f.raw(
            "tpm2_getpolicydigest",
            &[
                "-S",
                sess.to_str().unwrap(),
                "-o",
                out_file.to_str().unwrap()
            ],
        )
        .ok(),
        "could not read the trial digest"
    );
    let d = std::fs::read(&out_file).expect("read digest");
    let _ = f.raw("tpm2_flushcontext", &[sess.to_str().unwrap()]);
    d
}

/// Every policy digest this crate computes without a TPM, checked against the TPM's own
/// trial-session arithmetic: the floor authority computes approvals in software, which is only
/// safe while software and hardware agree, so that agreement is tested, not assumed.
fn assert_policy_digests_match_the_tpm(f: &Fixture, floor: counter::FloorState) {
    let nv_name = f.nv_name();
    // The index's Name, computed from its public area, must match the TPM's — including the
    // written bit, which flips on the first increment.
    assert_eq!(
        counter::written_name(f.cfg.nv_index, &f.nv_auth_policy),
        nv_name,
        "the computed written NV Name must match the TPM's"
    );
    assert_ne!(
        counter::unwritten_name(f.cfg.nv_index, &f.nv_auth_policy),
        nv_name,
        "the unwritten Name must differ, or the written bit is not in the public area"
    );

    let file = |name: &str, bytes: &[u8]| {
        let path = f.dir().join(name);
        std::fs::write(&path, bytes).expect("write a policy input");
        path.display().to_string()
    };
    let target = floor.counter + 1;
    let operand = file("purity_operand.bin", &7u64.to_be_bytes());
    let before_target = file("purity_target.bin", &(target - 1).to_be_bytes());
    let key_name = file("purity_key.name", &f.authority_name);
    let policy_ref = file("purity_ref.bin", approval::POLICY_REF);
    let signed = file("purity_a.bin", &counter::signed_branch(&f.authority_name));
    let first = file("purity_b.bin", &counter::first_write_branch());
    let sess = f.dir().join("purity.ctx").display().to_string();
    let idx = format!("0x{:08X}", f.cfg.nv_index);
    let step = |tool: &str, args: &[&str]| -> Vec<String> {
        [tool, "-S", &sess]
            .iter()
            .chain(args)
            .map(|a| (*a).to_string())
            .collect()
    };
    let increment = step("tpm2_policycommandcode", &["TPM2_CC_NV_Increment"]);
    let nv =
        |operand: &str, op: &str| step("tpm2_policynv", &["-i", operand, &idx, op, "-C", &idx]);
    let cases: Vec<DigestCase<'_>> = vec![
        (
            "PolicyCommandCode",
            vec![increment.clone()],
            tpm_policy::command_code(tpm_policy::ZERO, tpm_policy::CC_NV_INCREMENT).to_vec(),
        ),
        (
            "PolicyAuthValue",
            vec![step("tpm2_policyauthvalue", &[])],
            tpm_policy::auth_value(tpm_policy::ZERO).to_vec(),
        ),
        (
            "PolicyNvWritten(clear)",
            vec![step("tpm2_policynvwritten", &["c"])],
            tpm_policy::nv_written(tpm_policy::ZERO, false).to_vec(),
        ),
        (
            "PolicyNvWritten(set)",
            vec![step("tpm2_policynvwritten", &["s"])],
            tpm_policy::nv_written(tpm_policy::ZERO, true).to_vec(),
        ),
        (
            "PolicyNV(eq)",
            vec![nv(&operand, "eq")],
            tpm_policy::nv_counter(tpm_policy::ZERO, &nv_name, 7, tpm_policy::Operation::Eq)
                .to_vec(),
        ),
        (
            "PolicyNV(ule)",
            vec![nv(&operand, "ule")],
            tpm_policy::nv_counter(
                tpm_policy::ZERO,
                &nv_name,
                7,
                tpm_policy::Operation::UnsignedLe,
            )
            .to_vec(),
        ),
        (
            "PolicyAuthorize with the floor policy reference",
            vec![step(
                "tpm2_policyauthorize",
                &["-n", &key_name, "-q", &policy_ref],
            )],
            counter::signed_branch(&f.authority_name).to_vec(),
        ),
        (
            "the one-time materialise branch",
            vec![step("tpm2_policynvwritten", &["c"]), increment.clone()],
            counter::first_write_branch().to_vec(),
        ),
        (
            "PolicyOR of the two branches is the index's authPolicy",
            vec![step(
                "tpm2_policyor",
                &["-l", &format!("sha256:{signed},{first}")],
            )],
            f.nv_auth_policy.to_vec(),
        ),
        (
            "the policy an approval authorises",
            vec![increment, nv(&before_target, "eq")],
            approval::approved_policy(&nv_name, target)
                .unwrap()
                .to_vec(),
        ),
    ];
    for (what, steps, computed) in cases {
        assert_eq!(trial_digest(f, &steps), computed, "{what}");
    }
}

/// The TPM side of a machine's rollback protection, without a disk: the floor moves only by
/// signed, single-use approvals from its authority for this index, the floor can be attested to a
/// verifier, an unseal needs a prediction signed for that generation and these measurements, the
/// dictionary-attack lockout can be made real, and the counter survives its index being deleted
/// and recreated.
#[test]
fn the_floor_moves_only_by_signed_approvals_and_an_unseal_needs_its_own_prediction() {
    use base64::Engine as _;

    let f = fixture!();
    let ws = f.ws();
    let floor = f.floor(3); // this performs the one permitted unapproved increment
    assert_policy_digests_match_the_tpm(&f, floor);
    let authority_key = Path::new(&f.cfg.pcr_public_key);
    let increment = |a: &approval::Approval| {
        counter::increment_approved(
            &f.runner,
            &ws,
            f.cfg.nv_index,
            authority_key,
            &f.authority_name,
            a,
        )
    };

    // An increment without an approval is refused.
    let idx = format!("0x{:08X}", f.cfg.nv_index);
    assert!(
        !f.raw("tpm2_nvincrement", &[&idx, "-C", &idx]).ok(),
        "an unauthorised increment must be refused — otherwise anything running as root can push \
         the floor above the running generation and force the machine onto its recovery key"
    );
    assert!(
        !f.raw("tpm2_nvincrement", &[&idx, "-C", "o"]).ok(),
        "owner authorisation must not increment the floor"
    );
    // The materialise escape works exactly once: the written flag is now set, so
    // PolicyNvWritten(clear) can never be satisfied again.
    let err = counter::increment_first(&f.runner, &ws, f.cfg.nv_index, &f.authority_name)
        .expect_err("the materialise branch must not work a second time");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("never been written") || msg.contains("materialising"),
        "{msg}"
    );
    assert_eq!(
        f.counter_now(),
        floor.counter,
        "the counter must not have moved"
    );

    // An approval signed by another key, or issued against another machine's index, is refused.
    let evil_priv = f.evil_priv();
    let nv_name = f.nv_name();
    let req = approval::build_request(f.cfg.nv_index, &nv_name, floor, &[], 3).unwrap();
    let wrong_signer = approval::issue(&f.runner, &ws, &req, &evil_priv, 4, true)
        .expect("the forger can produce a well-formed approval");
    assert_eq!(wrong_signer.len(), 1);
    assert_eq!(wrong_signer[0].target_counter, floor.counter + 1);
    let err =
        increment(&wrong_signer[0]).expect_err("an approval from the wrong key must be refused");
    assert!(
        format!("{err:#}").contains("not signed by the configured authority"),
        "{err:#}"
    );
    let mut other_name = nv_name.clone();
    other_name[10] ^= 0xFF;
    let req = approval::build_request(f.cfg.nv_index, &other_name, floor, &[], 3).unwrap();
    let foreign = approval::issue(&f.runner, &ws, &req, &f.sign_priv, 4, true).expect("issue");
    let err =
        increment(&foreign[0]).expect_err("an approval bound to another index must be refused");
    assert!(
        format!("{err:#}").contains("different NV index Name"),
        "{err:#}"
    );
    assert_eq!(
        f.counter_now(),
        floor.counter,
        "the counter must not have moved"
    );

    // Issuance refuses approvals that could strand a device, keeping a stash of approvals from
    // becoming a denial of service after a later compromise. Generations 5 and 6 have keyslots;
    // 6 is the highest floor that still leaves something to unseal.
    let req = approval::build_request(f.cfg.nv_index, &nv_name, floor, &[5, 6], 3).unwrap();
    assert_eq!(req.safe_target_floor, 6);
    let err = approval::issue(&f.runner, &ws, &req, &f.sign_priv, 7, false)
        .expect_err("approving past the newest enrolled generation must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("newest generation with a sealed keyslot"),
        "{msg}"
    );
    let forced = approval::issue(&f.runner, &ws, &req, &f.sign_priv, 7, true)
        .expect("the override should work when explicitly requested");
    assert_eq!(forced.len(), 4);
    let approvals =
        approval::issue(&f.runner, &ws, &req, &f.sign_priv, 6, false).expect("issue to bound");
    assert_eq!(approvals.len(), 3, "floor 3 -> 6 is three single-use steps");
    assert_eq!(approvals[0].target_floor, 4);
    assert_eq!(approvals[2].target_floor, 6);

    // Each approval is bound to one target: the second step's cannot be used first, so a held
    // batch can't skip ahead or move several steps at once; and once used, it is spent in the
    // TPM itself.
    let err = increment(&approvals[1]).expect_err("an out-of-order approval must be refused");
    assert!(format!("{err:#}").contains("does not apply"), "{err:#}");
    assert_eq!(f.counter_now(), floor.counter);
    increment(&approvals[0]).expect("the approval should work once");
    assert_eq!(f.counter_now(), floor.counter + 1);
    let err = increment(&approvals[0]).expect_err("a replayed approval must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already been used") || msg.contains("does not apply"),
        "the refusal should say the approval is spent: {msg}"
    );
    assert_eq!(
        f.counter_now(),
        floor.counter + 1,
        "a replay must not move the counter"
    );
    for a in &approvals[1..] {
        increment(a).expect("in-order approvals should work");
    }
    let floor = counter::FloorState {
        counter: f.counter_now(),
        base: floor.base,
    };
    assert_eq!(floor.floor_generation(), 6);

    // A verifier can have the floor certified by an attestation key, fresh to its nonce.
    let nonce = b"\x01\x02\x03\x04verifier-nonce";
    let certify = || {
        tilo_rollback::certify::certify_floor(
            &f.runner,
            &ws,
            f.cfg.nv_index,
            floor.base,
            nonce,
            f.dir(),
        )
        .expect("certify the floor")
    };
    let att = certify();
    assert_eq!(att.counter, f.counter_now());
    assert_eq!(att.floor_generation, 6);
    assert!(
        !att.attestation.is_empty(),
        "an attestation structure should be produced"
    );
    assert!(!att.signature.is_empty(), "a signature should be produced");
    assert!(
        !att.key_public.is_empty(),
        "the attestation key's public area should be produced"
    );
    // The verifier's nonce must be inside the attestation, or a captured one replays forever.
    assert!(
        att.attestation.windows(nonce.len()).any(|w| w == nonce),
        "the qualifying nonce must appear in the attestation's extraData"
    );
    f.bump_floor(2);
    let att2 = certify();
    assert_eq!(att2.floor_generation, 8);
    assert_ne!(att.attestation, att2.attestation);

    // Signed predictions: generation 12's token unseals only on a prediction its authority signed
    // for generation 12. `TPM2_PolicyAuthorize` discards the PCR-derived digest, so without the
    // generation in `policyRef` a token would accept any prediction the authority ever signed —
    // including an old image's own — letting the floor check pass against the newest token's
    // bound.
    let floor = counter::FloorState {
        counter: f.counter_now(),
        base: floor.base,
    };
    let secret = b"cccccccccccccccccccccccccccccccc";
    let for_12 = f.predict(12, &f.sign_priv, "as built");
    let for_3 = f.predict(3, &f.sign_priv, "gen 3");
    assert_eq!(
        for_12.approved_policy, for_3.approved_policy,
        "the two predictions must cover identical measurements for this step to mean anything"
    );
    assert_ne!(
        for_12.signature, for_3.signature,
        "the signatures must differ, or the generation is not in the signed message"
    );
    let token = f.seal_token(12, floor, std::slice::from_ref(&for_12), secret, None);
    assert_eq!(
        f.try_unseal(&token, None).expect("initial").as_deref(),
        Some(&secret[..]),
        "generation 12's own prediction must work"
    );
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut swapped = token.clone();
    swapped.predictions[0].signature = b64.encode(&for_3.signature);
    assert_eq!(
        f.try_unseal(&swapped, None)
            .expect("cross-generation prediction"),
        None,
        "a prediction signed for generation 3 must not satisfy generation 12's token — if this \
         ever passes, the generation binding is gone and the rollback floor can be satisfied by \
         any image the authority ever published"
    );
    let mut wrong_key = token.clone();
    wrong_key.predictions[0].signature = b64.encode(&f.predict(12, &evil_priv, "forged").signature);
    assert_eq!(
        f.try_unseal(&wrong_key, None).expect("forged prediction"),
        None,
        "a prediction signed by a key the token is not bound to must not unseal"
    );

    // A firmware or kernel update moves PCR 11: the old prediction stops applying, and the
    // updater ships a signature for the new expected state with the sealed blob untouched — the
    // difference between prompting for a passphrase on every firmware update and not.
    assert!(
        f.raw(
            "tpm2_pcrextend",
            &["11:sha256=0000000000000000000000000000000000000000000000000000000000000001"]
        )
        .ok(),
        "could not extend PCR 11"
    );
    assert_eq!(
        f.try_unseal(&token, None).expect("stale prediction"),
        None,
        "the old prediction must stop applying once the measurements change"
    );
    let after = f.predict(12, &f.sign_priv, "post-update");
    let mut updated = token.clone();
    updated.predictions = [for_12, after].iter().map(SignedPrediction::from).collect();
    assert_eq!(
        updated.sealed_private, token.sealed_private,
        "the sealed blob must not have been re-created"
    );
    assert_eq!(
        f.try_unseal(&updated, None)
            .expect("new prediction")
            .as_deref(),
        Some(&secret[..]),
        "a freshly signed prediction must satisfy the existing seal"
    );
    // A changed device PCR 7 stays bound: even a newly signed release prediction cannot erase it,
    // because PCR 7 is applied after PolicyAuthorize resets the digest.
    assert!(
        f.raw(
            "tpm2_pcrextend",
            &["7:sha256=0000000000000000000000000000000000000000000000000000000000000001"]
        )
        .ok()
    );
    let mut changed = token;
    changed.predictions = [f.predict(12, &f.sign_priv, "same-target-uki")]
        .iter()
        .map(SignedPrediction::from)
        .collect();
    assert!(
        f.try_unseal(&changed, None).is_err(),
        "changed device PCR7 must remain bound after release authorization"
    );

    // The lockout that makes a short PIN defensible. Whatever this TPM ships with, an empty reset
    // authorisation is disqualifying; tightened, and with a real one, a short PIN is defensible,
    // and an empty-auth reset is refused.
    let shipped = lockout::read(&f.runner).expect("read lockout");
    assert!(
        !shipped.lockout_auth_set,
        "a fresh TPM should have no reset authorisation set; this one does, so the assertion below \
         would not be testing anything"
    );
    assert!(
        !lockout::assess(shipped, lockout::Target::default()).pin_is_defensible(),
        "an empty dictionary-attack reset authorisation must be reported as disqualifying"
    );
    let target = lockout::Target {
        max_tries: 8,
        recovery_time: 7200,
        lockout_recovery: 600,
    };
    let tightened = lockout::tighten(&f.runner, target, None, f.dir()).expect("tighten");
    assert_eq!(tightened.max_tries, 8, "maxTries should have been applied");
    assert_eq!(tightened.recovery_time, 7200);
    assert_eq!(tightened.lockout_recovery, 600);
    let auth = lockout::new_reset_auth().expect("auth");
    lockout::set_reset_auth(&f.runner, &auth, None, f.dir()).expect("set reset auth");
    let now = lockout::read(&f.runner).expect("read again");
    assert!(
        now.lockout_auth_set,
        "the reset authorisation should now be set"
    );
    assert!(
        lockout::assess(now, target).pin_is_defensible(),
        "with a real reset authorisation and tightened parameters a short PIN should be defensible"
    );
    assert!(
        !f.raw("tpm2_dictionarylockout", &["--clear-lockout"]).ok(),
        "clearing the dictionary-attack counter with an empty authorisation must fail once \
         lockoutAuth is set — otherwise the throttle is bypassable and the PIN is decorative"
    );

    // Attack: an owner-authorised caller deletes and recreates the index hoping the counter
    // resets. A redefined index is unwritten again, so materialise reapplies, and the counter
    // still does not regress.
    let before = f.counter_now();
    assert!(
        f.raw("tpm2_nvundefine", &[&idx, "-C", "o"]).ok(),
        "could not undefine the index, so this step proves nothing"
    );
    let policy_file = f.dir().join("redefine_policy.bin");
    std::fs::write(&policy_file, f.nv_auth_policy).expect("write policy");
    assert!(
        f.raw(
            "tpm2_nvdefine",
            &[
                &idx,
                "-C",
                "o",
                "-a",
                counter::NV_ATTRS,
                "-L",
                policy_file.to_str().unwrap(),
            ],
        )
        .ok(),
        "could not redefine the index"
    );
    counter::increment_first(&f.runner, &ws, f.cfg.nv_index, &f.authority_name)
        .expect("materialise after redefine");
    let after = f.counter_now();
    assert!(
        after >= before,
        "the NV counter regressed from {before} to {after} across undefine/redefine — the floor \
         would be resettable and bounded rollback would be worthless"
    );
}
