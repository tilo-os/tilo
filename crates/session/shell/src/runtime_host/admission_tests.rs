use super::Session;
use std::{
    io,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::Command,
};
use tilo_identity_host::provisioning::{IdentityStorage, ensure_throwaway_test_identity};
use tilo_runtime::RuntimeServices as _;

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

#[test]
fn readonly_admission_starts_existing_owner_and_refuses_missing_foreign_or_tampered_identity() {
    const CHILD_PROFILE: &str = "TILO_NATIVE_ADMISSION_STARTUP_TEST_PROFILE";
    if let Some(profile) = std::env::var_os(CHILD_PROFILE) {
        let root = PathBuf::from(profile);
        let case = root.file_name().unwrap().to_str().unwrap();
        assert_eq!(
            rustix::process::geteuid().as_raw(),
            1000,
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
        match Session::start_with_identity(&root, &mut |_| {}, Some(1000)) {
            Ok(session) => {
                assert_eq!(case, "owner", "{case} identity was incorrectly admitted");
                assert!(!session.actor.is_empty());
                assert!(!session.room.is_empty());
                assert!(
                    session.action_host().is_ok(),
                    "the actual semantic broker must be available"
                );
                assert!(Session::start_with_identity(&root, &mut |_| {}, Some(1001)).is_err());
                assert!(
                    std::fs::write(root.join("config/tilo/identity/primary.json"), b"overwrite")
                        .is_err(),
                    "the identity projection must actually be readonly"
                );
                drop(session);
            }
            Err(error) => {
                assert_ne!(case, "owner", "existing owner was refused: {error}");
                drop(error);
            }
        }
        drop(environment);
        return;
    }
    // bwrap maps the child's uid 1000 onto whatever uid runs this test, root or
    // not, so the profiles created here are the child's own either way.
    let root = tempfile::tempdir().unwrap();
    for case in ["owner", "missing", "foreign", "tampered"] {
        let profile = profile(root.path(), case).unwrap();
        let primary = profile.join("config/tilo/identity/primary.json");
        let before = std::fs::read(&primary).ok();
        let output = Command::new("bwrap")
            .args(["--die-with-parent","--unshare-user","--uid","1000","--gid","1000",
                "--unshare-pid","--unshare-net","--ro-bind","/","/","--proc","/proc","--dev","/dev","--bind"])
            .arg(&profile).arg(&profile)
            .arg("--ro-bind").arg(profile.join("config")).arg(profile.join("config"))
            .arg("--ro-bind").arg(profile.join("identity")).arg(profile.join("identity"))
            .args(["--setenv",CHILD_PROFILE]).arg(&profile)
            .arg("--").arg(std::env::current_exe().unwrap())
            .args(["--exact","runtime_host::admission_tests::readonly_admission_starts_existing_owner_and_refuses_missing_foreign_or_tampered_identity","--nocapture"])
            .output().expect("native admission startup requires working bubblewrap");
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(&primary).ok(),
            before,
            "admission changed the {case} identity"
        );
    }
}
