//! An isolated admission profile (`--admission-daemon`) of an existing device
//! owner: the identity is verified and never created, renewed or written, and
//! only that owner's account may be served.
// The fixture runs a non-root owner under bubblewrap, which only Linux has.
#![cfg(target_os = "linux")]

use anyhow::{Context as _, Result, ensure};
use std::{
    io,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
};
use tilo_identity_host::provisioning::{IdentityStorage, ensure_throwaway_test_identity};
use tilo_runtime::RuntimeServices as _;
use tilo_runtime::standalone::Standalone;
use tilo_test_env::SpawnUnderEnvLock as _;

// The broker these services answer through decodes peer bytes in worker
// processes started from this executable, which is the test binary here.
tilo_automerge_isolate::test_worker!();

/// Names the profile a child process was started to admit.
const CHILD_PROFILE: &str = "TILO_NATIVE_ADMISSION_STARTUP_TEST_PROFILE";
/// Where the child records that it ran the admission to its end.
const CHILD_PROOF: &str = "TILO_NATIVE_ADMISSION_STARTUP_TEST_PROOF";
/// The test, as libtest names it within this integration test crate.
const TEST: &str =
    "readonly_admission_starts_existing_owner_and_refuses_missing_foreign_or_tampered_identity";
/// The identities the fixture admits or refuses.
const CASES: &[&str] = &["owner", "missing", "foreign", "tampered"];

/// A profile holding the throwaway test identity, or that identity `missing`,
/// `foreign` (another account's) or `tampered` with.
fn profile(root: &Path, case: &str) -> io::Result<PathBuf> {
    let profile = root.join(case);
    for name in ["", "config", "data", "cache", "state", "identity"] {
        let directory = profile.join(name);
        std::fs::create_dir_all(&directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let identity = profile.join("config/tilo/identity");
    let paths = tilo_user_identity::IdentityPaths::under(profile.join("identity"));
    ensure_throwaway_test_identity(&identity, &paths, IdentityStorage::Files)?;
    match case {
        "missing" => std::fs::remove_file(identity.join("primary.json"))?,
        "tampered" => {
            let mut certificate = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
                .map_err(|error| io::Error::other(error.to_string()))?;
            certificate.device_label.push_str("tampered");
            tilo_user_identity::persist::write_device_cert(&paths.device_cert, &certificate)
                .map_err(|error| io::Error::other(error.to_string()))?;
        }
        "foreign" => {
            let shared = root.join("shared");
            ensure_throwaway_test_identity(&shared, &paths, IdentityStorage::Files)?;
            std::fs::copy(shared.join("primary.json"), identity.join("shared.json"))?;
            std::fs::write(identity.join("active"), "shared\n")?;
            std::fs::set_permissions(
                identity.join("active"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        _ => {}
    }
    Ok(profile)
}

/// Admit the profile a child process was started for, as that owner, and
/// record that the admission ran to its end.
fn admit_as_child() -> Result<()> {
    let root = std::env::var_os(CHILD_PROFILE).context("the profile to admit")?;
    let root = PathBuf::from(root);
    let case = root
        .file_name()
        .and_then(|name| name.to_str())
        .context("the fixture case in the profile's name")?
        .to_owned();
    ensure!(
        rustix::process::geteuid().as_raw() == 1000,
        "fixture must observe an actual non-root owner"
    );
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment
        .set("XDG_CONFIG_HOME", root.join("config"))
        .set("XDG_DATA_HOME", root.join("data"))
        .set("XDG_CACHE_HOME", root.join("cache"))
        .set("XDG_STATE_HOME", root.join("state"))
        .set("TILO_IDENTITY_ROOT", root.join("identity"))
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    match Standalone::start_admission_reporting(&root, 1000, &mut |_| {}) {
        Ok(session) => {
            ensure!(case == "owner", "{case} identity was incorrectly admitted");
            ensure!(!session.actor().is_empty(), "the owner has no actor");
            ensure!(
                !session.workspace().is_empty(),
                "the owner has no workspace"
            );
            ensure!(
                session.action_host().is_ok(),
                "the actual semantic broker must be available"
            );
            ensure!(
                Standalone::start_admission_reporting(&root, 1001, &mut |_| {}).is_err(),
                "another account was admitted to the owner's device"
            );
            ensure!(
                std::fs::write(root.join("config/tilo/identity/primary.json"), b"overwrite")
                    .is_err(),
                "the identity projection must actually be readonly"
            );
            drop(session);
        }
        Err(error) => {
            ensure!(case != "owner", "existing owner was refused: {error}");
        }
    }
    drop(environment);
    let proof = std::env::var_os(CHILD_PROOF).context("where the child records its end")?;
    std::fs::write(proof, case).context("recording the admission's end")?;
    Ok(())
}

/// Admit the `case` profile as its owner under bubblewrap, and check that only
/// the owner's identity was admitted and that no admission wrote to it.
fn admit_case(root: &Path, case: &str) -> Result<()> {
    let profile = profile(root, case)?;
    let primary = profile.join("config/tilo/identity/primary.json");
    let before = std::fs::read(&primary).ok();
    let proof = profile.join("child-ran");
    let output = Command::new("bwrap")
        .args([
            "--die-with-parent",
            "--unshare-user",
            "--uid",
            "1000",
            "--gid",
            "1000",
            "--unshare-pid",
            "--unshare-net",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--bind",
        ])
        .arg(&profile)
        .arg(&profile)
        .arg("--ro-bind")
        .arg(profile.join("config"))
        .arg(profile.join("config"))
        .arg("--ro-bind")
        .arg(profile.join("identity"))
        .arg(profile.join("identity"))
        .args(["--setenv", CHILD_PROFILE])
        .arg(&profile)
        .args(["--setenv", CHILD_PROOF])
        .arg(&proof)
        .arg("--")
        .arg(std::env::current_exe().context("the test binary's path")?)
        .args(["--exact", TEST, "--nocapture"])
        .output_locked()
        .context("native admission startup requires working bubblewrap")?;
    ensure!(
        output.status.success(),
        "{case}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        std::fs::read_to_string(&proof).ok().as_deref() == Some(case),
        "{case}: the child did not run the admission"
    );
    ensure!(
        std::fs::read(&primary).ok() == before,
        "admission changed the {case} identity"
    );
    Ok(())
}

#[test]
fn readonly_admission_starts_existing_owner_and_refuses_missing_foreign_or_tampered_identity() {
    if std::env::var_os(CHILD_PROFILE).is_some() {
        admit_as_child().unwrap_or_else(|error| panic!("{error:#}"));
        return;
    }
    tilo_test_env::require_capability!(
        UserNamespace,
        tilo_test_env::bubblewrap_with_proc(),
        "the admission fixture runs itself as uid 1000 under bubblewrap with its own /proc"
    );
    // bwrap maps the child's uid 1000 onto whatever uid runs this test, root or
    // not, so the profiles created here are the child's own either way.
    let root = tempfile::tempdir().unwrap();
    for case in CASES {
        admit_case(root.path(), case).unwrap_or_else(|error| panic!("{error:#}"));
    }
}
