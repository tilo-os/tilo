//! Mechanism tests against an emulated TPM and a loopback-free LUKS2 image, driving the
//! production `policy`/`keyslots`/`counter` code paths without root, loop devices or a VM.
//! Mounting and `tilo-rollback unlock` on a booted machine are not covered.
//!
//! Requires `swtpm`, `tpm2-tools`, and `cryptsetup`; run with
//! `cargo test -p tilo-rollback --test mechanism -- --test-threads 1` (each test gets its own
//! `swtpm` since several drive dictionary-attack lockout, which a shared instance would leak).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard; the arithmetic \
              here crafts an adversarial (stale) counter value for a rollback \
              test, not real user data"
)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tilo_rollback::{
    Config, Pbkdf, approval, confirm, counter, exec, keyslots, lockout, policy,
    policy::{Prediction, Workspace},
    token::{RollbackToken, SealedObject, SignedPrediction},
    tpm_policy,
};

// Harness

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .is_ok_and(|s| s.success())
}

fn missing_tools() -> Vec<&'static str> {
    ["swtpm", "tpm2_startup", "cryptsetup"]
        .into_iter()
        .filter(|t| !have(t))
        .collect()
}

/// A free TCP port `n` for which `n + 1` is also free.
///
/// Binding then releasing is racy: two tests can grab the same pair, so the loser's `swtpm`
/// fails with "address already in use" and its test talks to the wrong TPM. Retry with a
/// fresh pair on failure.
fn free_port_pair() -> (u16, u16) {
    use std::sync::atomic::{AtomicU16, Ordering};
    static SKEW: AtomicU16 = AtomicU16::new(0);
    let skew = SKEW.fetch_add(2, Ordering::Relaxed);
    for attempt in 0..64u16 {
        let base = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().unwrap().port()
        };
        // Walk away from the kernel's suggestion by a per-caller amount so two
        // threads handed adjacent ephemeral ports do not converge on one pair.
        let base = base
            .checked_add(skew)
            .and_then(|b| b.checked_add(attempt.saturating_mul(2)))
            .unwrap_or(base);
        if base >= u16::MAX - 1 {
            continue;
        }
        let a = std::net::TcpListener::bind(("127.0.0.1", base));
        let b = std::net::TcpListener::bind(("127.0.0.1", base.saturating_add(1)));
        if a.is_ok() && b.is_ok() {
            return (base, base.saturating_add(1));
        }
    }
    panic!("could not find an adjacent free TCP port pair for swtpm");
}

struct Tpm {
    _state: tempfile::TempDir,
    port: u16,
    child: Child,
}

impl Tpm {
    fn start() -> Option<Tpm> {
        let missing = missing_tools();
        if !missing.is_empty() {
            eprintln!(
                "SKIP: this test needs {missing:?}, which are not installed. Install swtpm, tpm2-tools, \
                 and cryptsetup to run it."
            );
            return None;
        }
        // Retry on a fresh port pair: a lost race shows up as swtpm exiting immediately.
        let mut last = String::new();
        for _ in 0..8 {
            match Self::try_start() {
                Ok(tpm) => return Some(tpm),
                Err(why) => last = why,
            }
        }
        panic!("could not start swtpm after 8 attempts; last failure: {last}");
    }

    fn try_start() -> Result<Tpm, String> {
        let state = tempfile::tempdir().expect("tempdir");
        // tpm2-tools' `swtpm:` TCTI assumes the control channel is at `port + 1`; two unrelated
        // free ports produce a data channel that answers and a control channel that looks broken.
        let (port, ctrl) = free_port_pair();
        // No `--create-ek-cert`: minting one needs swtpm_localca, which fails in a test sandbox
        // and is not needed here.
        let setup_log = state.path().join("setup.log");
        let _ = Command::new("swtpm_setup")
            .args(["--tpm2", "--tpmstate"])
            .arg(state.path())
            .stdout(Stdio::from(
                std::fs::File::create(&setup_log).expect("setup log"),
            ))
            .stderr(Stdio::from(
                std::fs::File::options()
                    .append(true)
                    .open(&setup_log)
                    .expect("setup log"),
            ))
            .status();
        let log = state.path().join("swtpm.log");
        let child = Command::new("swtpm")
            .arg("socket")
            .arg("--tpm2")
            .arg(format!("--tpmstate=dir={}", state.path().display()))
            .arg(format!("--server=type=tcp,port={port}"))
            .arg(format!("--ctrl=type=tcp,port={ctrl}"))
            .arg("--flags=not-need-init,startup-clear")
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&log).expect("swtpm log")))
            .stderr(Stdio::from(
                std::fs::File::options()
                    .append(true)
                    .open(&log)
                    .expect("swtpm log"),
            ))
            .spawn()
            .expect("spawning swtpm");
        let mut tpm = Tpm {
            _state: state,
            port,
            child,
        };
        let mut last = String::new();
        // Readiness usually takes a few probes; 300 x 100 ms only bounds a hang.
        for _ in 0..300 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Already-exited swtpm means the port pair was taken; retry with a new one.
            if let Ok(Some(status)) = tpm.child.try_wait() {
                return Err(format!(
                    "swtpm on port {port} exited ({status}): {}",
                    std::fs::read_to_string(&log).unwrap_or_default().trim()
                ));
            }
            match Command::new("tpm2_getcap")
                .arg("properties-fixed")
                .env("TPM2TOOLS_TCTI", tpm.tcti())
                .output()
            {
                Ok(o) if o.status.success() => return Ok(tpm),
                Ok(o) => last = String::from_utf8_lossy(&o.stderr).into_owned(),
                Err(e) => last = format!("could not run tpm2_getcap: {e}"),
            }
        }
        Err(format!(
            "swtpm on port {port} never answered.\n--- swtpm log ---\n{}\n\
             --- swtpm_setup log ---\n{}\n--- last probe error ---\n{last}",
            std::fs::read_to_string(&log).unwrap_or_default(),
            std::fs::read_to_string(&setup_log).unwrap_or_default(),
        ))
    }

    fn tcti(&self) -> String {
        format!("swtpm:host=127.0.0.1,port={}", self.port)
    }
}

impl Drop for Tpm {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    _tpm: Tpm,
    scratch: tempfile::TempDir,
    runner: exec::Real,
    cfg: Config,
    /// Private half of the stand-in prediction key.
    sign_priv: PathBuf,
    /// Second, unauthorised key.
    evil_priv: PathBuf,
    key_name: Vec<u8>,
    /// The floor authority's key Name and the NV index authPolicy it implies.
    authority_name: Vec<u8>,
    nv_auth_policy: tpm_policy::Digest256,
    /// Counter base from the last `floor()` call, used to convert counter values into floor generations.
    base: std::cell::Cell<i64>,
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let tpm = Tpm::start()?;
        let scratch = tempfile::tempdir().expect("tempdir");
        let runner = exec::Real::new()
            .with_tcti(Some(&tpm.tcti()))
            .with_cwd(scratch.path());
        // Stand-in for keys/tpm2-pcr-signing.*.pem — the real private key is git-excluded.
        let sign_priv = scratch.path().join("sign.priv.pem");
        let sign_pub = scratch.path().join("sign.pub.pem");
        gen_rsa(&sign_priv, &sign_pub);
        let evil_priv = scratch.path().join("evil.priv.pem");
        let evil_pub = scratch.path().join("evil.pub.pem");
        gen_rsa(&evil_priv, &evil_pub);

        // One key serves as both prediction and floor authority (the shipped default); tests
        // needing the distinction use `evil_priv` as the wrong key.
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
            evil_priv,
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
    /// to move the floor as setup, the same override the CLI gates behind an explicit flag.
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

    /// Raise the floor by `times` steps, issuing the approvals needed to do it.
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

    /// Issue exactly the approvals needed to confirm `generation` up to `target_floor`, mirroring
    /// a real deployment: staging enrolls, then the authority approves.
    fn approve_up_to(&self, base: i64, generation: u64, target_floor: u64) {
        let floor = counter::FloorState {
            counter: self.counter_now(),
            base,
        };
        self.issue_approvals(floor, &[generation], target_floor, true);
    }

    /// Base from the most recent `floor()` call, recovered from the counter (materialised first).
    fn current_base(&self) -> i64 {
        self.base.get()
    }

    /// Sign a prediction over the machine's current measurements for one generation;
    /// `key` selects the authorised or unauthorised signer.
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

    /// Seal a secret for one generation and wrap it in a token, without touching disk.
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

/// A generation enrolled without a PIN.
fn unpinned(
    generation: u64,
    floor: counter::FloorState,
    predictions: &[Prediction],
) -> keyslots::Enrollment<'_> {
    keyslots::Enrollment {
        generation,
        floor,
        predictions,
        pin: None,
    }
}

/// A LUKS2 image on a plain file, slot 0 holding `recovery`.
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
        .status()
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

// The counter really is a floor that cannot move backwards

#[test]
fn the_floor_counter_survives_undefine_and_redefine() {
    let f = fixture!();
    let floor = f.floor(0);
    f.bump_floor(5);
    let before = f.counter_now();
    assert!(before >= floor.counter + 5);

    // Attack: an owner-authorised caller deletes and recreates the index hoping the counter resets.
    let idx = format!("0x{:08X}", f.cfg.nv_index);
    assert!(
        f.raw("tpm2_nvundefine", &[&idx, "-C", "o"]).ok(),
        "could not undefine the index, so the test proves nothing"
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
    // A redefined index is unwritten again, so materialise reapplies — and the counter still
    // does not regress.
    counter::increment_first(&f.runner, &f.ws(), f.cfg.nv_index, &f.authority_name)
        .expect("materialise after redefine");
    let after = f.counter_now();
    assert!(
        after >= before,
        "the NV counter regressed from {before} to {after} across undefine/redefine — the floor \
         would be resettable and bounded rollback would be worthless"
    );
}

// The floor gates the unseal, at exactly the right boundary

#[test]
fn the_floor_blocks_generations_below_it_and_admits_the_rest() {
    let f = fixture!();
    let floor = f.floor(5);
    assert_eq!(floor.floor_generation(), 5);

    let secret = b"0123456789abcdef0123456789abcdef";
    let p5 = f.predict(5, &f.sign_priv, "g5");
    let p7 = f.predict(7, &f.sign_priv, "g7");
    let t5 = f.seal_token(5, floor, &[p5], secret, None);
    let t7 = f.seal_token(7, floor, &[p7], secret, None);

    // floor == 5: generation 5 is the oldest permitted, so both work.
    assert_eq!(
        f.try_unseal(&t5, None).expect("g5 at floor 5").as_deref(),
        Some(&secret[..]),
        "generation 5 must unseal when the floor is exactly 5"
    );
    assert_eq!(
        f.try_unseal(&t7, None).expect("g7 at floor 5").as_deref(),
        Some(&secret[..])
    );

    // floor -> 6: generation 5 is now below the floor.
    f.bump_floor(1);
    let e = f
        .try_unseal(&t5, None)
        .expect_err("generation 5 must be refused once the floor passes it");
    let msg = format!("{e:#}");
    assert!(
        msg.contains("rollback floor"),
        "the refusal should name the floor, got: {msg}"
    );
    assert_eq!(
        f.try_unseal(&t7, None).expect("g7 at floor 6").as_deref(),
        Some(&secret[..]),
        "generation 7 must still unseal when the floor is 6"
    );

    // floor -> 8: above every enrolled generation, so nothing unseals — the state a mis-declared
    // floor would leave a machine in, which is why confirm refuses to create it.
    f.bump_floor(2);
    assert!(f.try_unseal(&t5, None).is_err());
    assert!(f.try_unseal(&t7, None).is_err());
}

// Signed policy: the point of it, and the trap in it

#[test]
fn a_new_signed_prediction_restores_unlock_after_the_measurements_change() {
    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let before = f.predict(7, &f.sign_priv, "as built");
    let token = f.seal_token(7, floor, std::slice::from_ref(&before), secret, None);
    assert_eq!(
        f.try_unseal(&token, None).expect("initial").as_deref(),
        Some(&secret[..])
    );

    // A firmware or kernel update moves PCR 11.
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

    // The updater ships a signature for the new expected state; the sealed blob is untouched —
    // the difference between prompting for a passphrase on every firmware update and not.
    let after = f.predict(7, &f.sign_priv, "post-update");
    let mut updated = token.clone();
    updated.predictions = [before, after].iter().map(SignedPrediction::from).collect();
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
}

#[test]
fn a_release_prediction_cannot_authorize_a_changed_device_platform_pcr() {
    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"platform-bound-secret-32-bytes!!!";
    let before = f.predict(7, &f.sign_priv, "target-uki");
    let token = f.seal_token(7, floor, &[before], secret, None);
    assert!(f.try_unseal(&token, None).expect("sanity").is_some());

    assert!(
        f.raw(
            "tpm2_pcrextend",
            &["7:sha256=0000000000000000000000000000000000000000000000000000000000000001"]
        )
        .ok()
    );
    // Even a newly signed PCR11 statement cannot erase the PCR7 constraint,
    // because PCR7 is applied after PolicyAuthorize resets the digest.
    let new_release_prediction = f.predict(7, &f.sign_priv, "same-target-uki");
    let mut changed = token;
    changed.predictions = [new_release_prediction]
        .iter()
        .map(SignedPrediction::from)
        .collect();
    assert!(
        f.try_unseal(&changed, None).is_err(),
        "changed device PCR7 must remain bound after release authorization"
    );
}

#[test]
fn a_prediction_signed_by_another_key_is_refused() {
    use base64::Engine as _;

    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let good = f.predict(7, &f.sign_priv, "good");
    let token = f.seal_token(7, floor, &[good], secret, None);
    assert!(f.try_unseal(&token, None).expect("sanity").is_some());

    let forged = f.predict(7, &f.evil_priv, "forged");
    let mut tampered = token;
    let b64 = base64::engine::general_purpose::STANDARD;
    tampered.predictions[0].signature = b64.encode(&forged.signature);
    assert_eq!(
        f.try_unseal(&tampered, None).expect("forged prediction"),
        None,
        "a prediction signed by a key the token is not bound to must not unseal"
    );
}

/// `TPM2_PolicyAuthorize` discards the PCR-derived digest, so without the generation in
/// `policyRef` a token would accept any prediction the authority ever signed — including an
/// old image's own — letting the floor check pass against the newest token's bound.
#[test]
fn a_prediction_signed_for_a_different_generation_cannot_unseal_this_one() {
    use base64::Engine as _;

    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"cccccccccccccccccccccccccccccccc";

    // Both predictions cover the same live measurements and differ only in generation,
    // isolating the policyRef binding from the PCR binding.
    let for_7 = f.predict(7, &f.sign_priv, "gen 7");
    let for_3 = f.predict(3, &f.sign_priv, "gen 3");
    assert_eq!(
        for_7.approved_policy, for_3.approved_policy,
        "the two predictions must cover identical measurements for this test to mean anything"
    );
    assert_ne!(
        for_7.signature, for_3.signature,
        "the signatures must differ, or the generation is not in the signed message"
    );

    let token7 = f.seal_token(7, floor, &[for_7], secret, None);
    assert!(
        f.try_unseal(&token7, None).expect("sanity").is_some(),
        "generation 7's own prediction must work"
    );

    let mut swapped = token7;
    let b64 = base64::engine::general_purpose::STANDARD;
    swapped.predictions[0].signature = b64.encode(&for_3.signature);
    assert_eq!(
        f.try_unseal(&swapped, None)
            .expect("cross-generation prediction"),
        None,
        "a prediction signed for generation 3 must not satisfy generation 7's token — if this ever \
         passes, the generation binding is gone and the rollback floor can be satisfied by any \
         image the authority ever published"
    );
}

// The PIN, and the lockout that makes a short one defensible

#[test]
fn a_wrong_pin_fails_and_counts_against_the_hardware_lockout() {
    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"dddddddddddddddddddddddddddddddd";
    let pred = f.predict(7, &f.sign_priv, "g7");
    let token = f.seal_token(7, floor, &[pred], secret, Some(b"1234"));

    assert_eq!(
        f.try_unseal(&token, Some(b"1234"))
            .expect("right pin")
            .as_deref(),
        Some(&secret[..]),
        "the correct PIN must unseal"
    );

    let before = lockout::read(&f.runner).expect("lockout before");
    let e = f
        .try_unseal(&token, Some(b"9999"))
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
}

#[test]
fn lockout_parameters_can_be_read_tightened_and_given_a_real_reset_authorisation() {
    let f = fixture!();
    let shipped = lockout::read(&f.runner).expect("read lockout");
    // Whatever this TPM ships with, an empty reset authorisation is disqualifying.
    let verdict = lockout::assess(shipped, lockout::Target::default());
    assert!(
        !shipped.lockout_auth_set,
        "a fresh TPM should have no reset authorisation set; this one does, so the assertion below \
         would not be testing anything"
    );
    assert!(
        !verdict.pin_is_defensible(),
        "an empty dictionary-attack reset authorisation must be reported as disqualifying"
    );

    let target = lockout::Target {
        max_tries: 8,
        recovery_time: 7200,
        lockout_recovery: 600,
    };
    let tightened = lockout::tighten(&f.runner, target, None).expect("tighten");
    assert_eq!(tightened.max_tries, 8, "maxTries should have been applied");
    assert_eq!(tightened.recovery_time, 7200);
    assert_eq!(tightened.lockout_recovery, 600);

    let auth = lockout::new_reset_auth().expect("auth");
    lockout::set_reset_auth(&f.runner, &auth, None).expect("set reset auth");
    let now = lockout::read(&f.runner).expect("read again");
    assert!(
        now.lockout_auth_set,
        "the reset authorisation should now be set"
    );
    assert!(
        lockout::assess(now, target).pin_is_defensible(),
        "with a real reset authorisation and tightened parameters a short PIN should be defensible"
    );

    // And the whole point: an empty-auth reset must now be refused.
    let empty = f.raw("tpm2_dictionarylockout", &["--clear-lockout"]);
    assert!(
        !empty.ok(),
        "clearing the dictionary-attack counter with an empty authorisation must fail once \
         lockoutAuth is set — otherwise the throttle is bypassable and the PIN is decorative"
    );
}

// Keyslots: the full lifecycle, and the header-restore attack

#[test]
fn a_generation_keyslot_round_trips_and_the_recovery_key_survives() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-mechanism-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let floor = f.floor(5);

    assert!(
        keyslots::verify_recovery(&f.runner, &ws, &img, recovery).expect("verify recovery"),
        "the recovery key must open the freshly formatted image"
    );

    let preds = vec![f.predict(7, &f.sign_priv, "g7")];
    let (slot, token_id) = keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(7, floor, &preds),
        recovery,
    )
    .expect("enroll generation 7");
    assert_ne!(slot, 0, "the recovery key's slot must not be reused");

    let tokens = keyslots::list_tokens(&f.runner, &img).expect("list tokens");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].0, token_id);
    assert_eq!(tokens[0].1.generation, 7);

    let secret = f
        .try_unseal(&tokens[0].1, None)
        .expect("unseal")
        .expect("a prediction should apply");
    assert!(
        keyslots::verify_passphrase(&f.runner, &ws, &img, &secret, Some(slot)).expect("verify"),
        "the unsealed secret must open the keyslot the token points at"
    );
    assert!(
        keyslots::verify_recovery(&f.runner, &ws, &img, recovery).expect("verify recovery again"),
        "the recovery key must still work after enrollment"
    );
}

#[test]
fn a_slot_is_never_removed_without_a_proven_alternative() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-removal-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let floor = f.floor(5);

    let p6 = vec![f.predict(6, &f.sign_priv, "g6")];
    let p7 = vec![f.predict(7, &f.sign_priv, "g7")];
    let (slot6, tok6) = keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(6, floor, &p6),
        recovery,
    )
    .expect("enroll 6");
    let (_slot7, _tok7) = keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(7, floor, &p7),
        recovery,
    )
    .expect("enroll 7");

    let tokens = keyslots::list_tokens(&f.runner, &img).expect("list");
    let t7 = &tokens
        .iter()
        .find(|(_, t)| t.generation == 7)
        .expect("token 7")
        .1;
    let proof7 = f.try_unseal(t7, None).expect("unseal 7").expect("applies");

    // A passphrase that does not open the device must not authorise a removal.
    let e = keyslots::kill_generation_slot(
        &f.runner,
        &ws,
        &img,
        tok6,
        slot6,
        b"not-the-right-passphrase",
    )
    .expect_err("a bogus proof must be refused");
    assert!(
        format!("{e:#}").contains("does not open"),
        "the refusal should say the proof does not open the device: {e:#}"
    );

    // Nor may a slot authorise its own removal.
    let t6 = &tokens
        .iter()
        .find(|(_, t)| t.generation == 6)
        .expect("token 6")
        .1;
    let proof6 = f.try_unseal(t6, None).expect("unseal 6").expect("applies");
    let e = keyslots::kill_generation_slot(&f.runner, &ws, &img, tok6, slot6, &proof6)
        .expect_err("self-authorised removal must be refused");
    assert!(
        format!("{e:#}").contains("that very slot"),
        "the refusal should name the self-authorisation: {e:#}"
    );

    // With a real, different, verified proof it goes.
    keyslots::kill_generation_slot(&f.runner, &ws, &img, tok6, slot6, &proof7)
        .expect("removal with a proven alternative");
    assert!(
        !keyslots::verify_passphrase(&f.runner, &ws, &img, &proof6, None).expect("verify"),
        "the removed slot must no longer open the device"
    );
    assert!(
        keyslots::verify_recovery(&f.runner, &ws, &img, recovery).expect("verify recovery"),
        "the recovery key must survive a prune"
    );
}

/// Restoring a LUKS2 header backup resurrects every pruned token and keyslot — why the
/// keyslot mechanism cannot stand alone and why the floor lives in the TPM.
#[test]
fn a_restored_header_resurrects_the_token_but_the_floor_still_refuses_it() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-header-attack";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let floor = f.floor(5);

    // Generation 5 is enrolled while it is inside the window.
    let p5 = vec![f.predict(5, &f.sign_priv, "g5")];
    let (slot5, tok5) = keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(5, floor, &p5),
        recovery,
    )
    .expect("enroll 5");
    let t5 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .find(|(_, t)| t.generation == 5)
        .expect("token 5")
        .1;
    let secret5 = f.try_unseal(&t5, None).expect("unseal 5").expect("applies");

    // The attacker snapshots the header while generation 5 is still valid.
    let hdr = f.dir().join("header.bin");
    assert!(
        f.raw(
            "cryptsetup",
            &[
                "luksHeaderBackup",
                "--header-backup-file",
                hdr.to_str().unwrap(),
                img.to_str().unwrap(),
            ],
        )
        .ok(),
        "luksHeaderBackup failed"
    );

    // Time passes: generation 8 is confirmed, the floor moves past 5, and 5's keyslot is pruned.
    let p8 = vec![f.predict(8, &f.sign_priv, "g8")];
    let (_slot8, _tok8) = keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(8, floor, &p8),
        recovery,
    )
    .expect("enroll 8");
    let t8 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .find(|(_, t)| t.generation == 8)
        .expect("token 8")
        .1;
    let proof8 = f.try_unseal(&t8, None).expect("unseal 8").expect("applies");
    keyslots::kill_generation_slot(&f.runner, &ws, &img, tok5, slot5, &proof8).expect("prune 5");
    f.bump_floor(3); // floor 5 -> 8
    assert_eq!(
        counter::FloorState {
            counter: f.counter_now(),
            base: floor.base
        }
        .floor_generation(),
        8
    );

    // The attack: put the old header back.
    assert!(
        f.raw(
            "cryptsetup",
            &[
                "luksHeaderRestore",
                "--batch-mode",
                "--header-backup-file",
                hdr.to_str().unwrap(),
                img.to_str().unwrap(),
            ],
        )
        .ok(),
        "luksHeaderRestore failed"
    );

    // The keyslot mechanism alone is defeated: token and keyslot are back, old passphrase works.
    let resurrected = keyslots::list_tokens(&f.runner, &img).expect("list after restore");
    assert!(
        resurrected.iter().any(|(_, t)| t.generation == 5),
        "the header restore should have brought generation 5's token back — if it did not, this \
         test is not exercising the attack it claims to"
    );
    assert!(
        keyslots::verify_passphrase(&f.runner, &ws, &img, &secret5, None).expect("verify"),
        "the resurrected keyslot should accept its old passphrase"
    );

    // What still holds: the TPM won't release that passphrase, because the floor lives in the
    // TPM and the header restore did not touch it.
    let t5_again = resurrected
        .into_iter()
        .find(|(_, t)| t.generation == 5)
        .expect("token 5 back")
        .1;
    let e = f
        .try_unseal(&t5_again, None)
        .expect_err("the floor must refuse the resurrected generation");
    assert!(
        format!("{e:#}").contains("rollback floor"),
        "the refusal should come from the floor: {e:#}"
    );
}

// The confirmation sequence, end to end

/// Exercised through `confirm::run`, the same function the CLI calls, so the ordering under
/// test matches the ordering the machine uses.
#[test]
fn confirming_a_generation_enrolls_it_prunes_the_window_and_then_raises_the_floor() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-confirm-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");

    // Generations 5, 6, 7, 8 all ship a prediction for the live measurements.
    for g in [5u64, 6, 7, 8] {
        let p = f.predict(g, &f.sign_priv, "");
        policy::write_signed_prediction(&preds, g, &f.cfg.pcr_bank, &f.cfg.pcrs, &p)
            .expect("write prediction");
    }

    // First enrollment: generation 5, floor at the bottom of the window.
    let floor = f.floor(3);
    let (slot5, _t5) = keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(5, floor, &[f.predict(5, &f.sign_priv, "")]),
        recovery,
    )
    .expect("enroll 5");
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(5),
            foreign_keyslots: vec![0],
        },
    )
    .expect("seed state");
    assert!(slot5 != 0);

    let req = |generation: u64, floor_at_least: Option<u64>, dry_run: bool| confirm::Request {
        cfg: &f.cfg,
        device: &img,
        generation,
        prediction_dir: &preds,
        recovery: keyslots::RecoveryCheck::Key(recovery),
        pin: None,
        proof: None,
        floor_at_least,
        state_path: &state,
        scratch: f.dir(),
        dry_run,
    };

    // A dry run must not move anything.
    let counter_before = f.counter_now();
    let dry = confirm::run(&f.runner, &ws, &req(6, None, true)).expect("dry run");
    assert_eq!(dry.floor.floor_generation(), 3);
    assert_eq!(
        f.counter_now(),
        counter_before,
        "a dry run must not touch the counter"
    );
    assert!(dry.enrolled.is_none());
    assert!(dry.dropped.is_empty());

    // Confirm 6 and 7: the 3-generation window still holds 5, 6, 7, so the floor rises to 5 and
    // nothing is pruned; each advance spends approvals the authority issued for it.
    f.approve_up_to(floor.base, 6, 4);
    let o6 = confirm::run(&f.runner, &ws, &req(6, None, false)).expect("confirm 6");
    assert!(
        o6.enrolled.is_some(),
        "generation 6 should have been enrolled"
    );
    assert_eq!(o6.floor.floor_generation(), 4);
    assert!(o6.dropped.is_empty());

    f.approve_up_to(floor.base, 7, 5);
    let o7 = confirm::run(&f.runner, &ws, &req(7, None, false)).expect("confirm 7");
    assert_eq!(o7.floor.floor_generation(), 5);
    assert!(o7.dropped.is_empty(), "5, 6 and 7 all fit the window");

    // Confirm 8: 5 falls out of the window, so its keyslot goes and the floor moves to 6.
    f.approve_up_to(floor.base, 8, 6);
    let o8 = confirm::run(&f.runner, &ws, &req(8, None, false)).expect("confirm 8");
    assert_eq!(o8.dropped, vec![5], "generation 5 should have been pruned");
    assert_eq!(o8.floor.floor_generation(), 6);

    // The pruned generation's keyslot is gone; only the window's generations remain.
    let left: Vec<u64> = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .map(|(_, t)| t.generation)
        .collect();
    assert_eq!(left, vec![8, 7, 6]);

    // Generation 6 is the floor, so it must still unseal; anything below must not.
    let t6 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .find(|(_, t)| t.generation == 6)
        .expect("token 6")
        .1;
    assert!(
        f.try_unseal(&t6, None).expect("unseal 6").is_some(),
        "the generation at the floor must still unseal"
    );

    // And the recovery key, at every step, still works.
    assert!(keyslots::verify_recovery(&f.runner, &ws, &img, recovery).expect("verify recovery"));
}

#[test]
fn a_declared_floor_closes_the_window_on_a_vulnerable_generation() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-declared-floor-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");
    for g in [7u64, 8] {
        let p = f.predict(g, &f.sign_priv, "");
        policy::write_signed_prediction(&preds, g, &f.cfg.pcr_bank, &f.cfg.pcrs, &p)
            .expect("write");
    }
    let floor = f.floor(5);
    keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(7, floor, &[f.predict(7, &f.sign_priv, "")]),
        recovery,
    )
    .expect("enroll 7");
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(7),
            foreign_keyslots: vec![0],
        },
    )
    .expect("seed state");
    let t7 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .remove(0)
        .1;

    // Generation 8 is a security fix, 7 vulnerable: declaring floor 8 closes the window
    // immediately rather than leaving 7 reachable — a deliberate jump the authority approves.
    f.approve_up_to(floor.base, 8, 8);
    let o = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 8,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::Key(recovery),
            pin: None,
            proof: None,
            floor_at_least: Some(8),
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
    .expect("confirm 8 with a declared floor");
    assert_eq!(
        o.plan.floor.window_floor, 6,
        "the window alone would have allowed 6"
    );
    assert_eq!(o.floor.floor_generation(), 8);

    // Generation 7's keyslot is still in the header (inside the retention window); the floor is
    // what refuses it — presence is not permission.
    assert!(
        keyslots::list_tokens(&f.runner, &img)
            .expect("list")
            .iter()
            .any(|(_, t)| t.generation == 7),
        "generation 7's token should still be present"
    );
    let e = f
        .try_unseal(&t7, None)
        .expect_err("the declared floor must refuse generation 7");
    assert!(format!("{e:#}").contains("rollback floor"), "{e:#}");
    assert!(keyslots::verify_recovery(&f.runner, &ws, &img, recovery).expect("verify recovery"));
}

#[test]
fn a_declared_floor_above_the_running_generation_is_refused_before_anything_moves() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-bad-declaration-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");
    let p = f.predict(7, &f.sign_priv, "");
    policy::write_signed_prediction(&preds, 7, &f.cfg.pcr_bank, &f.cfg.pcrs, &p).expect("write");
    let floor = f.floor(5);
    keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(7, floor, &[p]),
        recovery,
    )
    .expect("enroll 7");
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(7),
            foreign_keyslots: vec![0],
        },
    )
    .expect("seed state");

    let before = f.counter_now();
    let e = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 7,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::Key(recovery),
            pin: None,
            proof: None,
            floor_at_least: Some(9),
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
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
}

// Attestation

#[test]
fn the_floor_can_be_certified_with_an_attestation_key() {
    let f = fixture!();
    let floor = f.floor(5);
    let nonce = b"\x01\x02\x03\x04verifier-nonce";
    let att = tilo_rollback::certify::certify_floor(
        &f.runner,
        &f.ws(),
        f.cfg.nv_index,
        floor.base,
        nonce,
        f.dir(),
    )
    .expect("certify the floor");

    assert_eq!(att.counter, floor.counter);
    assert_eq!(att.floor_generation, 5);
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

    // Moving the floor must change what is certified.
    f.bump_floor(2);
    let att2 = tilo_rollback::certify::certify_floor(
        &f.runner,
        &f.ws(),
        f.cfg.nv_index,
        floor.base,
        nonce,
        f.dir(),
    )
    .expect("certify again");
    assert_eq!(att2.floor_generation, 7);
    assert_ne!(att.attestation, att2.attestation);
}

// The unattended path — the one the machine actually takes after a reboot

/// After a reboot nothing knows the recovery key or PIN; confirmation must work from the
/// secret boot itself unlocked with, plus the independent keyslots still being present.
#[test]
fn an_unattended_confirmation_works_from_the_boot_secret_alone() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-unattended-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");
    for g in [6u64, 7] {
        let p = f.predict(g, &f.sign_priv, "");
        policy::write_signed_prediction(&preds, g, &f.cfg.pcr_bank, &f.cfg.pcrs, &p)
            .expect("write");
    }

    // Enrolled with a PIN, the way a real machine is.
    let pin: &[u8] = b"482913";
    let floor = f.floor(4);
    keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &keyslots::Enrollment {
            generation: 6,
            floor,
            predictions: &[f.predict(6, &f.sign_priv, "")],
            pin: Some(pin),
        },
        recovery,
    )
    .expect("enroll 6 with a PIN");
    keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &keyslots::Enrollment {
            generation: 7,
            floor,
            predictions: &[f.predict(7, &f.sign_priv, "")],
            pin: Some(pin),
        },
        recovery,
    )
    .expect("enroll 7 with a PIN");
    let foreign = keyslots::foreign_keyslots(&f.runner, &img).expect("foreign slots");
    assert_eq!(
        foreign.len(),
        1,
        "only the recovery key's slot should be foreign: {foreign:?}"
    );
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(6),
            foreign_keyslots: foreign.clone(),
        },
    )
    .expect("seed state");

    // What boot does: unseal generation 7 with the PIN, keep the secret.
    let t7 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .find(|(_, t)| t.generation == 7)
        .expect("token 7")
        .1;
    let boot_secret = f
        .try_unseal(&t7, Some(pin))
        .expect("unseal 7 with the PIN")
        .expect("a prediction applies");

    // The authority has approved the advance this confirmation will make.
    f.approve_up_to(floor.base, 7, 5);

    // What confirmation does: no recovery key, no PIN, just the secret boot used.
    let o = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 7,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::ForeignSlotsPresent { expected: &foreign },
            pin: None,
            proof: Some(&boot_secret),
            floor_at_least: None,
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
    .expect("unattended confirmation");
    assert_eq!(o.floor.floor_generation(), 5);
    assert!(keyslots::verify_recovery(&f.runner, &ws, &img, recovery).expect("verify recovery"));
}

#[test]
fn an_unattended_confirmation_refuses_a_proof_that_is_not_that_generations_secret() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-wrong-proof-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");
    for g in [6u64, 7] {
        let p = f.predict(g, &f.sign_priv, "");
        policy::write_signed_prediction(&preds, g, &f.cfg.pcr_bank, &f.cfg.pcrs, &p)
            .expect("write");
    }
    let floor = f.floor(4);
    for g in [6u64, 7] {
        keyslots::enroll_generation(
            &f.runner,
            &ws,
            &img,
            &f.cfg,
            &unpinned(g, floor, &[f.predict(g, &f.sign_priv, "")]),
            recovery,
        )
        .expect("enroll");
    }
    let foreign = keyslots::foreign_keyslots(&f.runner, &img).expect("foreign");
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(6),
            foreign_keyslots: foreign.clone(),
        },
    )
    .expect("seed state");

    // Generation 6's secret must not prove generation 7: otherwise booting an older image could
    // confirm a newer one and drag the floor past what the machine can actually run.
    let t6 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .find(|(_, t)| t.generation == 6)
        .expect("token 6")
        .1;
    let secret6 = f.try_unseal(&t6, None).expect("unseal 6").expect("applies");
    let before = f.counter_now();
    let e = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 7,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::ForeignSlotsPresent { expected: &foreign },
            pin: None,
            proof: Some(&secret6),
            floor_at_least: None,
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
    .expect_err("the wrong generation's secret must not authorise a confirmation");
    assert!(
        format!("{e:#}").contains("does not open generation 7's keyslot"),
        "{e:#}"
    );
    assert_eq!(f.counter_now(), before, "the counter must not have moved");
}

#[test]
fn a_confirmation_refuses_to_proceed_when_the_recovery_keyslot_has_vanished() {
    let f = fixture!();
    let recovery = b"recovery-key-that-gets-removed";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");
    let p7 = f.predict(7, &f.sign_priv, "");
    policy::write_signed_prediction(&preds, 7, &f.cfg.pcr_bank, &f.cfg.pcrs, &p7).expect("write");
    let floor = f.floor(5);
    keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(7, floor, &[p7]),
        recovery,
    )
    .expect("enroll 7");
    let foreign = keyslots::foreign_keyslots(&f.runner, &img).expect("foreign");
    assert_eq!(foreign, vec![0]);
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(7),
            foreign_keyslots: foreign.clone(),
        },
    )
    .expect("seed state");
    let t7 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .remove(0)
        .1;
    let secret7 = f.try_unseal(&t7, None).expect("unseal").expect("applies");

    // Something outside this tool removes the recovery keyslot.
    let kf = f.dir().join("kill.key");
    std::fs::write(&kf, &secret7).expect("write key file");
    assert!(
        f.raw(
            "cryptsetup",
            &[
                "luksKillSlot",
                "--batch-mode",
                "--key-file",
                kf.to_str().unwrap(),
                img.to_str().unwrap(),
                "0",
            ],
        )
        .ok(),
        "could not remove slot 0, so this test proves nothing"
    );

    let before = f.counter_now();
    let e = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 7,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::ForeignSlotsPresent { expected: &foreign },
            pin: None,
            proof: Some(&secret7),
            floor_at_least: None,
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
    .expect_err("a missing independent keyslot must stop the confirmation");
    let msg = format!("{e:#}");
    assert!(
        msg.contains("no longer in") || msg.contains("no keyslot outside"),
        "{msg}"
    );
    assert_eq!(f.counter_now(), before, "the counter must not have moved");
}

/// The staged update's keyslot must survive confirming the currently-running generation.
///
/// Regression test: `drop_set` treated any generation newer than the running one as stale
/// residue, but a machine mid-update normally runs N with N+1's keyslot sealed. Confirming N
/// deleted N+1's keyslot, so rebooting into N+1 had nothing to unseal and fell back to the
/// recovery key.
#[test]
fn confirming_the_running_generation_keeps_the_staged_generations_keyslot() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-staging-test";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");
    for g in [5u64, 6] {
        let p = f.predict(g, &f.sign_priv, "");
        policy::write_signed_prediction(&preds, g, &f.cfg.pcr_bank, &f.cfg.pcrs, &p)
            .expect("write");
    }
    let floor = f.floor(3);

    // Running generation 5, and generation 6 staged with its keyslot sealed.
    keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(5, floor, &[f.predict(5, &f.sign_priv, "")]),
        recovery,
    )
    .expect("enroll 5");
    let (slot6, _) = keyslots::enroll_generation(
        &f.runner,
        &ws,
        &img,
        &f.cfg,
        &unpinned(6, floor, &[f.predict(6, &f.sign_priv, "")]),
        recovery,
    )
    .expect("stage 6");
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(5),
            foreign_keyslots: vec![0],
        },
    )
    .expect("seed state");

    let o = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 5,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::Key(recovery),
            pin: None,
            proof: None,
            floor_at_least: None,
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
    .expect("confirm 5");
    assert!(
        o.dropped.is_empty(),
        "confirming 5 must not drop anything, least of all the staged 6: {:?}",
        o.dropped
    );

    let left: Vec<u64> = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .map(|(_, t)| t.generation)
        .collect();
    assert!(
        left.contains(&6),
        "generation 6's token must survive: {left:?}"
    );
    assert!(
        keyslots::keyslots_in_use(&f.runner, &img)
            .expect("slots")
            .contains(&slot6),
        "generation 6's keyslot must survive"
    );

    // And it still unseals, so the reboot into 6 has something to unlock with.
    let t6 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .find(|(_, t)| t.generation == 6)
        .expect("token 6")
        .1;
    assert!(
        f.try_unseal(&t6, None).expect("unseal 6").is_some(),
        "the staged generation's secret must still be releasable"
    );
}

// The floor cannot be advanced without a signed, single-use approval

/// Every policy digest this crate computes without a TPM, checked against the TPM's own
/// trial-session arithmetic — the floor authority computes approvals in software, which is
/// only safe while software and hardware agree, so that agreement is tested, not assumed.
/// A trial session is the TPM's own answer for the policy `steps` given. Every hand-computed
/// digest in [`pure_policy_digests_match_the_tpms_own_arithmetic`] is checked against it.
fn trial_digest(f: &Fixture, steps: &[&[&str]]) -> Vec<u8> {
    let sess = f.dir().join("purity.ctx");
    let _ = f.raw("tpm2_flushcontext", &["-s"]);
    assert!(
        f.raw("tpm2_startauthsession", &["-S", sess.to_str().unwrap()])
            .ok(),
        "could not start a trial session"
    );
    for step in steps {
        let mut argv: Vec<&str> = vec![];
        argv.extend_from_slice(step);
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

/// `PolicyCommandCode`, `PolicyAuthValue` and `PolicyNvWritten` (both ways) each match the TPM's own
/// trial-session digest.
fn assert_simple_policy_primitives_match(f: &Fixture, sess_s: &str) {
    assert_eq!(
        trial_digest(
            f,
            &[&[
                "tpm2_policycommandcode",
                "-S",
                sess_s,
                "TPM2_CC_NV_Increment"
            ]]
        ),
        tpm_policy::command_code(tpm_policy::ZERO, tpm_policy::CC_NV_INCREMENT).to_vec(),
        "PolicyCommandCode"
    );

    assert_eq!(
        trial_digest(f, &[&["tpm2_policyauthvalue", "-S", sess_s]]),
        tpm_policy::auth_value(tpm_policy::ZERO).to_vec(),
        "PolicyAuthValue"
    );

    // PolicyNvWritten, both ways.
    assert_eq!(
        trial_digest(f, &[&["tpm2_policynvwritten", "-S", sess_s, "c"]]),
        tpm_policy::nv_written(tpm_policy::ZERO, false).to_vec(),
        "PolicyNvWritten(clear)"
    );
    assert_eq!(
        trial_digest(f, &[&["tpm2_policynvwritten", "-S", sess_s, "s"]]),
        tpm_policy::nv_written(tpm_policy::ZERO, true).to_vec(),
        "PolicyNvWritten(set)"
    );
}

/// `PolicyNV` (both comparison operations we rely on) matches the TPM's own trial-session
/// digest. Returns the operand file path, reused by the end-to-end approval check.
fn assert_policy_nv_matches(f: &Fixture, sess_s: &str, idx: &str, nv_name: &[u8]) -> PathBuf {
    let operand = f.dir().join("purity_operand.bin");
    std::fs::write(&operand, 7u64.to_be_bytes()).expect("write operand");
    let op_s = operand.to_str().unwrap().to_string();
    assert_eq!(
        trial_digest(
            f,
            &[&[
                "tpm2_policynv",
                "-S",
                sess_s,
                "-i",
                &op_s,
                idx,
                "eq",
                "-C",
                idx
            ]]
        ),
        tpm_policy::nv_counter(tpm_policy::ZERO, nv_name, 7, tpm_policy::Operation::Eq).to_vec(),
        "PolicyNV(eq)"
    );
    assert_eq!(
        trial_digest(
            f,
            &[&[
                "tpm2_policynv",
                "-S",
                sess_s,
                "-i",
                &op_s,
                idx,
                "ule",
                "-C",
                idx
            ]]
        ),
        tpm_policy::nv_counter(
            tpm_policy::ZERO,
            nv_name,
            7,
            tpm_policy::Operation::UnsignedLe
        )
        .to_vec(),
        "PolicyNV(ule)"
    );
    operand
}

/// `PolicyAuthorize` (the signed branch) and the one-time materialise branch each match the
/// TPM's own trial-session digest, and `PolicyOR` of the two equals the index's authPolicy.
fn assert_policy_authorize_and_or_match(f: &Fixture, sess_s: &str) {
    let name_file = f.dir().join("purity_key.name");
    std::fs::write(&name_file, &f.authority_name).expect("write key name");
    let ref_file = f.dir().join("purity_ref.bin");
    std::fs::write(&ref_file, approval::POLICY_REF).expect("write ref");
    assert_eq!(
        trial_digest(
            f,
            &[&[
                "tpm2_policyauthorize",
                "-S",
                sess_s,
                "-n",
                name_file.to_str().unwrap(),
                "-q",
                ref_file.to_str().unwrap(),
            ]]
        ),
        counter::signed_branch(&f.authority_name).to_vec(),
        "PolicyAuthorize with the floor policy reference"
    );
    assert_eq!(
        trial_digest(
            f,
            &[
                &["tpm2_policynvwritten", "-S", sess_s, "c"],
                &[
                    "tpm2_policycommandcode",
                    "-S",
                    sess_s,
                    "TPM2_CC_NV_Increment"
                ],
            ]
        ),
        counter::first_write_branch().to_vec(),
        "the one-time materialise branch"
    );

    // PolicyOR over those two branches is the index's authPolicy.
    let a_file = f.dir().join("purity_a.bin");
    let b_file = f.dir().join("purity_b.bin");
    std::fs::write(&a_file, counter::signed_branch(&f.authority_name)).expect("write a");
    std::fs::write(&b_file, counter::first_write_branch()).expect("write b");
    let list = format!("sha256:{},{}", a_file.display(), b_file.display());
    assert_eq!(
        trial_digest(f, &[&["tpm2_policyor", "-S", sess_s, "-l", &list]]),
        f.nv_auth_policy.to_vec(),
        "PolicyOR of the two branches is the index's authPolicy"
    );
}

/// The end-to-end approval policy — command code + `PolicyNV` for `target - 1` — matches what
/// `approval::approved_policy` computes. `operand` is the file [`assert_policy_nv_matches`]
/// already created; it gets overwritten here with `target - 1`.
fn assert_approval_policy_matches(
    f: &Fixture,
    sess_s: &str,
    idx: &str,
    nv_name: &[u8],
    operand: &Path,
    target: u64,
) {
    let op_s = operand.to_str().unwrap().to_string();
    std::fs::write(operand, (target - 1).to_be_bytes()).expect("write operand");
    assert_eq!(
        trial_digest(
            f,
            &[
                &[
                    "tpm2_policycommandcode",
                    "-S",
                    sess_s,
                    "TPM2_CC_NV_Increment"
                ],
                &[
                    "tpm2_policynv",
                    "-S",
                    sess_s,
                    "-i",
                    &op_s,
                    idx,
                    "eq",
                    "-C",
                    idx
                ],
            ]
        ),
        approval::approved_policy(nv_name, target).unwrap().to_vec(),
        "the policy an approval authorises"
    );
}

#[test]
fn pure_policy_digests_match_the_tpms_own_arithmetic() {
    let f = fixture!();
    let floor = f.floor(3);
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

    let sess = f.dir().join("purity.ctx");
    let sess_s = sess.to_str().unwrap().to_string();
    let idx = format!("0x{:08X}", f.cfg.nv_index);

    assert_simple_policy_primitives_match(&f, &sess_s);
    let operand = assert_policy_nv_matches(&f, &sess_s, &idx, &nv_name);
    assert_policy_authorize_and_or_match(&f, &sess_s);

    // And the approval policy, end to end.
    let target = floor.counter + 1;
    assert_approval_policy_matches(&f, &sess_s, &idx, &nv_name, &operand, target);
}

#[test]
fn an_increment_without_an_approval_is_refused() {
    let f = fixture!();
    let floor = f.floor(3);
    let idx = format!("0x{:08X}", f.cfg.nv_index);

    // Empty authorisation: what a root process could try on its own (index has no authwrite).
    let out = f.raw("tpm2_nvincrement", &[&idx, "-C", &idx]);
    assert!(
        !out.ok(),
        "an unauthorised increment must be refused — otherwise anything running as root can push \
         the floor above the running generation and force the machine onto its recovery key"
    );
    // Owner authorisation must not work either.
    let out = f.raw("tpm2_nvincrement", &[&idx, "-C", "o"]);
    assert!(
        !out.ok(),
        "owner authorisation must not increment the floor"
    );
    assert_eq!(
        f.counter_now(),
        floor.counter,
        "the counter must not have moved"
    );
}

#[test]
fn the_materialise_escape_works_exactly_once() {
    let f = fixture!();
    let floor = f.floor(3); // this performs the one permitted unapproved increment
    assert_eq!(f.counter_now(), floor.counter);

    // The written flag is now set, so PolicyNvWritten(clear) can never be satisfied again.
    let err = counter::increment_first(&f.runner, &f.ws(), f.cfg.nv_index, &f.authority_name)
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
}

#[test]
fn a_replayed_approval_is_refused_by_the_tpm() {
    let f = fixture!();
    let ws = f.ws();
    let floor = f.floor(3);
    let approvals = f.issue_approvals(floor, &[], 4, false);
    assert_eq!(approvals.len(), 1, "one step, one approval");
    let approval = &approvals[0];

    // First use works.
    counter::increment_approved(
        &f.runner,
        &ws,
        f.cfg.nv_index,
        Path::new(&f.cfg.pcr_public_key),
        &f.authority_name,
        approval,
    )
    .expect("the approval should work once");
    let after = f.counter_now();
    assert_eq!(after, floor.counter + 1);

    // Replaying the same approval must fail in the TPM itself — the file is handed straight back in.
    let err = counter::increment_approved(
        &f.runner,
        &ws,
        f.cfg.nv_index,
        Path::new(&f.cfg.pcr_public_key),
        &f.authority_name,
        approval,
    )
    .expect_err("a replayed approval must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("already been used") || msg.contains("does not apply"),
        "the refusal should say the approval is spent: {msg}"
    );
    assert_eq!(f.counter_now(), after, "a replay must not move the counter");
}

#[test]
fn an_approval_is_bound_to_one_target_and_cannot_reach_another() {
    let f = fixture!();
    let ws = f.ws();
    let floor = f.floor(3);
    // Approvals for two steps: floor 4 and floor 5.
    let approvals = f.issue_approvals(floor, &[], 5, false);
    assert_eq!(approvals.len(), 2);

    // The second step's approval cannot be used first (counter isn't at target minus one yet),
    // so a held batch can't be used to skip ahead or move several steps at once.
    let second = approvals
        .iter()
        .find(|a| a.target_counter == floor.counter + 2)
        .expect("second-step approval");
    let err = counter::increment_approved(
        &f.runner,
        &ws,
        f.cfg.nv_index,
        Path::new(&f.cfg.pcr_public_key),
        &f.authority_name,
        second,
    )
    .expect_err("an out-of-order approval must be refused");
    assert!(format!("{err:#}").contains("does not apply"), "{err:#}");
    assert_eq!(f.counter_now(), floor.counter);

    // In order, both work, one step each.
    for a in &approvals {
        counter::increment_approved(
            &f.runner,
            &ws,
            f.cfg.nv_index,
            Path::new(&f.cfg.pcr_public_key),
            &f.authority_name,
            a,
        )
        .expect("in-order approvals should work");
    }
    assert_eq!(f.counter_now(), floor.counter + 2);
}

#[test]
fn an_approval_signed_by_another_key_is_refused() {
    let f = fixture!();
    let ws = f.ws();
    let floor = f.floor(3);
    let nv_name = f.nv_name();
    let target = floor.counter + 1;

    // Same policy, same target, wrong signer.
    let req = approval::build_request(f.cfg.nv_index, &nv_name, floor, &[], 3).unwrap();
    let forged = approval::issue(&f.runner, &ws, &req, &f.evil_priv, 4, true)
        .expect("the forger can produce a well-formed approval");
    assert_eq!(forged.len(), 1);
    assert_eq!(forged[0].target_counter, target);

    let err = counter::increment_approved(
        &f.runner,
        &ws,
        f.cfg.nv_index,
        Path::new(&f.cfg.pcr_public_key),
        &f.authority_name,
        &forged[0],
    )
    .expect_err("an approval from the wrong key must be refused");
    assert!(
        format!("{err:#}").contains("not signed by the configured authority"),
        "{err:#}"
    );
    assert_eq!(
        f.counter_now(),
        floor.counter,
        "the counter must not have moved"
    );
}

/// Enroll generations 5-8 (predictions + keyslots) into a fresh image and seed `state.json`
/// as already confirmed through 5 — the starting point
/// [`without_approvals_the_floor_stays_put_and_the_disk_still_unlocks`] confirms generation 8
/// against.
fn setup_generations_5_to_8(
    f: &Fixture,
    recovery: &[u8],
) -> (Workspace, PathBuf, PathBuf, PathBuf, counter::FloorState) {
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let preds = f.dir().join("predictions");
    let state = f.dir().join("state.json");
    for g in [5u64, 6, 7, 8] {
        let p = f.predict(g, &f.sign_priv, "");
        policy::write_signed_prediction(&preds, g, &f.cfg.pcr_bank, &f.cfg.pcrs, &p)
            .expect("write");
    }
    let floor = f.floor(3);
    for g in [5u64, 6, 7, 8] {
        keyslots::enroll_generation(
            &f.runner,
            &ws,
            &img,
            &f.cfg,
            &unpinned(g, floor, &[f.predict(g, &f.sign_priv, "")]),
            recovery,
        )
        .expect("enroll");
    }
    confirm::save_state(
        &state,
        &confirm::State {
            nv_index: f.cfg.nv_index,
            nv_base: floor.base,
            last_confirmed: Some(5),
            foreign_keyslots: keyslots::foreign_keyslots(&f.runner, &img).expect("foreign"),
        },
    )
    .expect("seed state");
    (ws, img, preds, state, floor)
}

/// The answer to "what if the authority is unavailable": the floor stops
/// advancing, the disk keeps unlocking, and the keyslot window is still pruned.
#[test]
fn without_approvals_the_floor_stays_put_and_the_disk_still_unlocks() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-no-approvals-test";
    let (ws, img, preds, state, floor) = setup_generations_5_to_8(&f, recovery);

    // No approvals anywhere.
    assert!(
        approval::load_all(&f.approval_dir())
            .expect("load")
            .is_empty(),
        "this test needs to start with no approvals"
    );

    let o = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 8,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::Key(recovery),
            pin: None,
            proof: None,
            floor_at_least: None,
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
    .expect("confirmation must succeed even with no approvals");

    assert_eq!(
        o.floor.floor_generation(),
        3,
        "the floor must not have moved without an approval"
    );
    assert!(
        !o.unapproved_steps.is_empty(),
        "the unapproved steps must be reported rather than passed over silently"
    );
    assert_eq!(
        o.dropped,
        vec![5],
        "the keyslot window must still be pruned"
    );

    // The disk still unlocks — the whole point.
    let t8 = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .find(|(_, t)| t.generation == 8)
        .expect("token 8")
        .1;
    assert!(
        f.try_unseal(&t8, None).expect("unseal 8").is_some(),
        "an unavailable floor authority must never stop the disk unlocking"
    );
    assert!(keyslots::verify_recovery(&f.runner, &ws, &img, recovery).expect("verify recovery"));

    // And once approvals arrive, the same confirmation advances.
    let floor_now = counter::FloorState {
        counter: f.counter_now(),
        base: floor.base,
    };
    let enrolled: Vec<u64> = keyslots::list_tokens(&f.runner, &img)
        .expect("list")
        .into_iter()
        .map(|(_, t)| t.generation)
        .collect();
    f.issue_approvals(floor_now, &enrolled, 6, true);
    let o2 = confirm::run(
        &f.runner,
        &ws,
        &confirm::Request {
            cfg: &f.cfg,
            device: &img,
            generation: 8,
            prediction_dir: &preds,
            recovery: keyslots::RecoveryCheck::Key(recovery),
            pin: None,
            proof: None,
            floor_at_least: None,
            state_path: &state,
            scratch: f.dir(),
            dry_run: false,
        },
    )
    .expect("confirmation with approvals");
    assert_eq!(o2.floor.floor_generation(), 6);
    assert!(o2.unapproved_steps.is_empty(), "{:?}", o2.unapproved_steps);
}

/// Issuance refuses to hand out approvals that could strand a device — keeping a stash of
/// approvals from becoming a denial-of-service after a later compromise.
#[test]
fn issuance_refuses_to_approve_past_the_newest_enrolled_generation() {
    let f = fixture!();
    let ws = f.ws();
    let floor = f.floor(3);
    let nv_name = f.nv_name();

    // Generations 5 and 6 have keyslots; 6 is the highest floor that still leaves something to unseal.
    let req = approval::build_request(f.cfg.nv_index, &nv_name, floor, &[5, 6], 3).unwrap();
    assert_eq!(req.safe_target_floor, 6);

    let err = approval::issue(&f.runner, &ws, &req, &f.sign_priv, 7, false)
        .expect_err("approving past the newest enrolled generation must be refused");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("newest generation with a sealed keyslot"),
        "{msg}"
    );

    // Up to the bound is fine, and it is exactly enough steps to get there.
    let ok = approval::issue(&f.runner, &ws, &req, &f.sign_priv, 6, false).expect("issue to bound");
    assert_eq!(ok.len(), 3, "floor 3 -> 6 is three single-use steps");
    assert_eq!(ok[0].target_floor, 4);
    assert_eq!(ok[2].target_floor, 6);

    // And the override exists, but has to be asked for.
    let forced = approval::issue(&f.runner, &ws, &req, &f.sign_priv, 7, true)
        .expect("the override should work when explicitly requested");
    assert_eq!(forced.len(), 4);
}

#[test]
fn an_approval_for_another_machines_index_is_refused() {
    let f = fixture!();
    let ws = f.ws();
    let floor = f.floor(3);

    // Same authority, same target, but issued against a different NV Name.
    let mut other_name = f.nv_name();
    other_name[10] ^= 0xFF;
    let req = approval::build_request(f.cfg.nv_index, &other_name, floor, &[], 3).unwrap();
    let foreign = approval::issue(&f.runner, &ws, &req, &f.sign_priv, 4, true).expect("issue");

    let err = counter::increment_approved(
        &f.runner,
        &ws,
        f.cfg.nv_index,
        Path::new(&f.cfg.pcr_public_key),
        &f.authority_name,
        &foreign[0],
    )
    .expect_err("an approval bound to another index must be refused");
    assert!(
        format!("{err:#}").contains("different NV index Name"),
        "{err:#}"
    );
    assert_eq!(f.counter_now(), floor.counter);
}
