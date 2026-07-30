//! Mechanism tests against an emulated TPM and a loopback-free LUKS2 image.
//!
//! These are the tests that decide whether bounded rollback actually bounds
//! anything. Everything here drives the production code paths — the same
//! `policy`, `keyslots` and `counter` functions the `qos-rollback` binary calls —
//! against a real TPM 2.0 implementation (`swtpm`) and a real LUKS2 header.
//!
//! # Why a file image and not a loopback device
//!
//! `cryptsetup` operates on a regular file for everything that only touches the
//! header: `luksFormat`, `luksAddKey`, `luksKillSlot`, `token import/export`, and
//! `open --test-passphrase`, which does the full key-derivation and digest check
//! without creating a device-mapper mapping. So the whole keyslot lifecycle is
//! testable without root, without loop devices, and without a VM.
//!
//! What that does *not* cover is the boot path: mounting the mapping, and early
//! userspace calling `qos-rollback unlock`. That needs a booted machine and is
//! called out as unverified rather than implied by these passing.
//!
//! # Requirements
//!
//! `swtpm`, `tpm2-tools`, `cryptsetup` and `openssl`. When they are absent each
//! test prints what is missing and returns; running them is
//!
//! ```text
//! cargo test -p qos-rollback --test mechanism -- --test-threads 1
//! ```
//!
//! Each test gets its own `swtpm` instance, because several of them deliberately
//! drive the TPM into dictionary-attack lockout and a shared instance would leak
//! that into its neighbours.

use qos_rollback::{
    Config, Pbkdf, counter, exec, keyslots, lockout, policy,
    policy::{Prediction, Workspace},
    token::RollbackToken,
};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn have(tool: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {tool} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn missing_tools() -> Vec<&'static str> {
    ["swtpm", "tpm2_startup", "cryptsetup", "openssl"]
        .into_iter()
        .filter(|t| !have(t))
        .collect()
}

/// A free TCP port `n` for which `n + 1` is also free.
fn free_port_pair() -> (u16, u16) {
    for _ in 0..64 {
        let base = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().unwrap().port()
        };
        if base == u16::MAX {
            continue;
        }
        if std::net::TcpListener::bind(("127.0.0.1", base + 1)).is_ok() {
            return (base, base + 1);
        }
    }
    panic!("could not find an adjacent free TCP port pair for swtpm");
}

/// A private `swtpm` instance.
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
                "SKIP: this test needs {:?}, which are not installed. Install swtpm, tpm2-tools, \
                 cryptsetup and openssl to run it.",
                missing
            );
            return None;
        }
        let state = tempfile::tempdir().expect("tempdir");
        // tpm2-tools' `swtpm:` TCTI takes ONE port and assumes the control
        // channel is at `port + 1`. Allocating two unrelated free ports produces
        // a data channel that answers and a control channel that refuses, which
        // shows up as "Could not set locality" and looks like a broken TPM.
        let (port, ctrl) = free_port_pair();
        // No `--create-ek-cert`: minting one shells out to swtpm_localca, which
        // wants /var/lib/swtpm-localca and fails in a test sandbox. Nothing here
        // needs an EK certificate.
        let setup_log = state.path().join("setup.log");
        let _ = Command::new("swtpm_setup")
            .args(["--tpm2", "--tpmstate"])
            .arg(state.path())
            .stdout(Stdio::from(std::fs::File::create(&setup_log).expect("setup log")))
            .stderr(Stdio::from(
                std::fs::File::options().append(true).open(&setup_log).expect("setup log"),
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
                std::fs::File::options().append(true).open(&log).expect("swtpm log"),
            ))
            .spawn()
            .expect("spawning swtpm");
        let tpm = Tpm {
            _state: state,
            port,
            child,
        };
        // Wait for it to answer.
        let mut last = String::new();
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let r = Command::new("tpm2_getcap")
                .arg("properties-fixed")
                .env("TPM2TOOLS_TCTI", tpm.tcti())
                .output();
            match r {
                Ok(o) if o.status.success() => return Some(tpm),
                Ok(o) => last = String::from_utf8_lossy(&o.stderr).into_owned(),
                Err(e) => last = format!("could not run tpm2_getcap: {e}"),
            }
        }
        panic!(
            "swtpm did not become reachable on port {port}\n--- swtpm log ---\n{}\n             --- swtpm_setup log ---\n{}\n--- last probe error ---\n{last}",
            std::fs::read_to_string(&log).unwrap_or_default(),
            std::fs::read_to_string(&setup_log).unwrap_or_default(),
        );
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

/// Everything one test needs.
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
}

impl Fixture {
    fn new() -> Option<Fixture> {
        let tpm = Tpm::start()?;
        let scratch = tempfile::tempdir().expect("tempdir");
        let runner = exec::Real::new()
            .with_tcti(Some(&tpm.tcti()))
            .with_cwd(scratch.path());
        // A stand-in for keys/tpm2-pcr-signing.*.pem. The real private key is not
        // in the tree (it is git-excluded), and a test must not depend on it.
        let sign_priv = scratch.path().join("sign.priv.pem");
        let sign_pub = scratch.path().join("sign.pub.pem");
        gen_rsa(&sign_priv, &sign_pub);
        let evil_priv = scratch.path().join("evil.priv.pem");
        let evil_pub = scratch.path().join("evil.pub.pem");
        gen_rsa(&evil_priv, &evil_pub);

        let mut cfg = Config {
            pcr_public_key: sign_pub.display().to_string(),
            pbkdf: Some(Pbkdf {
                algorithm: "pbkdf2".into(),
                iterations: 1000,
            }),
            ..Config::default()
        };
        cfg.nv_index = counter::DEFAULT_NV_INDEX;

        let ws = Workspace::new(scratch.path());
        let key_name = policy::key_name(&runner, &ws, &sign_pub).expect("key name");
        Some(Fixture {
            _tpm: tpm,
            scratch,
            runner,
            cfg,
            sign_priv,
            evil_priv,
            key_name,
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

    /// Set up the floor counter with the floor at `initial_floor`.
    fn floor(&self, initial_floor: u64) -> counter::FloorState {
        counter::define(&self.runner, self.cfg.nv_index).expect("define");
        counter::materialise(&self.runner, self.cfg.nv_index, self.dir(), initial_floor)
            .expect("materialise")
    }

    /// Sign a prediction over the machine's *current* measurements, for one
    /// generation. `key` selects the authorised or the unauthorised signer.
    fn predict(&self, generation: u64, key: &Path, label: &str) -> Prediction {
        let ws = self.ws();
        let approved =
            policy::approved_policy(&self.runner, &ws, &self.selection(), None).expect("approved");
        let sig = policy::sign_prediction(&self.runner, &ws, key, &approved, generation)
            .expect("sign");
        Prediction {
            approved_policy: approved,
            signature: sig,
            label: label.into(),
        }
    }

    /// Seal a secret for one generation and wrap it in a token, without touching
    /// any disk.
    fn seal_token(
        &self,
        generation: u64,
        floor: counter::FloorState,
        preds: &[Prediction],
        secret: &[u8],
        pin: Option<&[u8]>,
    ) -> RollbackToken {
        let ws = self.ws();
        let operand = floor.operand_for(generation).expect("operand");
        let digest = policy::seal_policy(
            &self.runner,
            &ws,
            &self.key_name,
            generation,
            self.cfg.nv_index,
            operand,
            pin.is_some(),
        )
        .expect("seal policy");
        let (pu, pr) = keyslots::seal(&self.runner, &ws, secret, &digest, pin).expect("seal");
        RollbackToken::new(
            0,
            generation,
            self.cfg.nv_index,
            operand,
            floor.base,
            &self.cfg.pcr_bank,
            &self.cfg.pcrs,
            pin.is_some(),
            &pu,
            &pr,
            &self.key_name,
            &digest,
            preds,
        )
    }

    fn try_unseal(&self, token: &RollbackToken, pin: Option<&[u8]>) -> anyhow::Result<Option<Vec<u8>>> {
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

    fn bump_floor(&self, times: u64) {
        for _ in 0..times {
            counter::increment(&self.runner, self.cfg.nv_index).expect("increment");
        }
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
    let ok = Command::new("openssl")
        .args(["genrsa", "-out"])
        .arg(priv_path)
        .arg("2048")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("openssl genrsa")
        .success();
    assert!(ok, "openssl genrsa failed");
    let ok = Command::new("openssl")
        .args(["rsa", "-in"])
        .arg(priv_path)
        .arg("-pubout")
        .arg("-out")
        .arg(pub_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("openssl rsa")
        .success();
    assert!(ok, "openssl rsa -pubout failed");
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

// ---------------------------------------------------------------------------
// The counter really is a floor that cannot move backwards
// ---------------------------------------------------------------------------

#[test]
fn the_floor_counter_survives_undefine_and_redefine() {
    let f = fixture!();
    let floor = f.floor(0);
    f.bump_floor(5);
    let before = f.counter_now();
    assert!(before >= floor.counter + 5);

    // The attack: an owner-authorised caller deletes the index and recreates it,
    // hoping for a counter that starts over.
    let idx = format!("0x{:08X}", f.cfg.nv_index);
    assert!(
        f.raw("tpm2_nvundefine", &[&idx, "-C", "o"]).ok(),
        "could not undefine the index, so the test proves nothing"
    );
    assert!(
        f.raw("tpm2_nvdefine", &[&idx, "-C", "o", "-a", counter::NV_ATTRS]).ok(),
        "could not redefine the index"
    );
    counter::increment(&f.runner, f.cfg.nv_index).expect("increment after redefine");
    let after = f.counter_now();
    assert!(
        after >= before,
        "the NV counter regressed from {before} to {after} across undefine/redefine — the floor \
         would be resettable and bounded rollback would be worthless"
    );
}

// ---------------------------------------------------------------------------
// The floor gates the unseal, at exactly the right boundary
// ---------------------------------------------------------------------------

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

    // floor -> 8: above every enrolled generation. Nothing unseals — this is the
    // state a mis-declared floor would leave a machine in, and why the confirm
    // path refuses to create it.
    f.bump_floor(2);
    assert!(f.try_unseal(&t5, None).is_err());
    assert!(f.try_unseal(&t7, None).is_err());
}

// ---------------------------------------------------------------------------
// Signed policy: the point of it, and the trap in it
// ---------------------------------------------------------------------------

#[test]
fn a_new_signed_prediction_restores_unlock_after_the_measurements_change() {
    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let before = f.predict(7, &f.sign_priv, "as built");
    let token = f.seal_token(7, floor, &[before.clone()], secret, None);
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

    // The updater ships a signature for the new expected state. The sealed blob
    // is untouched — this is the difference between a machine that asks for a
    // passphrase after every firmware update and one that does not.
    let after = f.predict(7, &f.sign_priv, "post-update");
    let mut updated = token.clone();
    updated.predictions = RollbackToken::new(
        0,
        7,
        f.cfg.nv_index,
        token.nv_operand,
        token.nv_base,
        &f.cfg.pcr_bank,
        &f.cfg.pcrs,
        false,
        b"x",
        b"y",
        &f.key_name,
        b"z",
        &[before, after],
    )
    .predictions;
    assert_eq!(
        updated.sealed_private, token.sealed_private,
        "the sealed blob must not have been re-created"
    );
    assert_eq!(
        f.try_unseal(&updated, None).expect("new prediction").as_deref(),
        Some(&secret[..]),
        "a freshly signed prediction must satisfy the existing seal"
    );
}

#[test]
fn a_prediction_signed_by_another_key_is_refused() {
    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let good = f.predict(7, &f.sign_priv, "good");
    let token = f.seal_token(7, floor, &[good], secret, None);
    assert!(f.try_unseal(&token, None).expect("sanity").is_some());

    let forged = f.predict(7, &f.evil_priv, "forged");
    let mut tampered = token.clone();
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    tampered.predictions[0].signature = b64.encode(&forged.signature);
    assert_eq!(
        f.try_unseal(&tampered, None).expect("forged prediction"),
        None,
        "a prediction signed by a key the token is not bound to must not unseal"
    );
}

/// The subtle one. `TPM2_PolicyAuthorize` discards the PCR-derived digest, so
/// without the generation in `policyRef` a token would accept *any* prediction
/// the authority ever signed — including an old image's own published one — and
/// the floor check would then pass against the newest token's bound.
#[test]
fn a_prediction_signed_for_a_different_generation_cannot_unseal_this_one() {
    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"cccccccccccccccccccccccccccccccc";

    // Both predictions cover the *same* measurements — the live ones. They differ
    // only in the generation they were signed for. That is what isolates the
    // policyRef binding from the PCR binding.
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

    let mut swapped = token7.clone();
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    swapped.predictions[0].signature = b64.encode(&for_3.signature);
    assert_eq!(
        f.try_unseal(&swapped, None).expect("cross-generation prediction"),
        None,
        "a prediction signed for generation 3 must not satisfy generation 7's token — if this ever \
         passes, the generation binding is gone and the rollback floor can be satisfied by any \
         image the authority ever published"
    );
}

// ---------------------------------------------------------------------------
// The PIN, and the lockout that makes a short one defensible
// ---------------------------------------------------------------------------

#[test]
fn a_wrong_pin_fails_and_counts_against_the_hardware_lockout() {
    let f = fixture!();
    let floor = f.floor(0);
    let secret = b"dddddddddddddddddddddddddddddddd";
    let pred = f.predict(7, &f.sign_priv, "g7");
    let token = f.seal_token(7, floor, &[pred], secret, Some(b"1234"));

    assert_eq!(
        f.try_unseal(&token, Some(b"1234")).expect("right pin").as_deref(),
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

// ---------------------------------------------------------------------------
// Keyslots: the full lifecycle, and the header-restore attack
// ---------------------------------------------------------------------------

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
        &f.runner, &ws, &img, &f.cfg, 7, floor, &preds, recovery, None,
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
    let (slot6, tok6) =
        keyslots::enroll_generation(&f.runner, &ws, &img, &f.cfg, 6, floor, &p6, recovery, None)
            .expect("enroll 6");
    let (_slot7, _tok7) =
        keyslots::enroll_generation(&f.runner, &ws, &img, &f.cfg, 7, floor, &p7, recovery, None)
            .expect("enroll 7");

    let tokens = keyslots::list_tokens(&f.runner, &img).expect("list");
    let t7 = &tokens.iter().find(|(_, t)| t.generation == 7).expect("token 7").1;
    let proof7 = f.try_unseal(t7, None).expect("unseal 7").expect("applies");

    // A passphrase that does not open the device must not authorise a removal.
    let e = keyslots::kill_generation_slot(
        &f.runner, &ws, &img, tok6, slot6, b"not-the-right-passphrase",
    )
    .expect_err("a bogus proof must be refused");
    assert!(
        format!("{e:#}").contains("does not open"),
        "the refusal should say the proof does not open the device: {e:#}"
    );

    // Nor may a slot authorise its own removal.
    let t6 = &tokens.iter().find(|(_, t)| t.generation == 6).expect("token 6").1;
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

/// Restoring a LUKS2 header backup resurrects every pruned token and keyslot.
/// This is why the retained-keyslot mechanism cannot stand alone, and why the
/// floor lives in the TPM.
#[test]
fn a_restored_header_resurrects_the_token_but_the_floor_still_refuses_it() {
    let f = fixture!();
    let recovery = b"recovery-key-for-the-header-attack";
    let img = luks_image(f.dir(), recovery);
    let ws = f.ws();
    let floor = f.floor(5);

    // Generation 5 is enrolled while it is inside the window.
    let p5 = vec![f.predict(5, &f.sign_priv, "g5")];
    let (slot5, tok5) =
        keyslots::enroll_generation(&f.runner, &ws, &img, &f.cfg, 5, floor, &p5, recovery, None)
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

    // Time passes. Generation 8 is confirmed, the floor moves past 5, and 5's
    // keyslot is pruned.
    let p8 = vec![f.predict(8, &f.sign_priv, "g8")];
    let (_slot8, _tok8) =
        keyslots::enroll_generation(&f.runner, &ws, &img, &f.cfg, 8, floor, &p8, recovery, None)
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
        counter::FloorState { counter: f.counter_now(), base: floor.base }.floor_generation(),
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

    // The keyslot mechanism alone has been defeated: the token and its keyslot
    // are back, and the old passphrase opens the device again.
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

    // And the mechanism that still holds: the TPM will not release that
    // passphrase, because the floor is in the TPM and the header restore did not
    // touch it.
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
