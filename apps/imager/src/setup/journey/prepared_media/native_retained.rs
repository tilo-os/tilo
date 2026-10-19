//! Actual owned GPT/nonroot frontend/standalone root-helper producer test.
//! No synthetic disk facts or caller receipt are supplied. The dedicated
//! test-only elevation bridge exercises kernel credentials + canonical helper;
//! distribution Polkit policy is intentionally not qualified.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Permissions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(feature = "native-capture-qualification")]
mod actor {
    use super::*;
    #[test]
    fn shared_files_media_authenticates_original_plan_without_copying_home() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let root = PathBuf::from(
            std::env::var_os("TILO_NATIVE_SHARED_FILES_ACTOR_ROOT")
                .expect("explicit owned native actor fixture required"),
        );
        assert_eq!(rustix::process::getuid().as_raw(), 1000);
        assert_eq!(rustix::process::geteuid().as_raw(), 1000);
        assert_eq!(rustix::process::getgid().as_raw(), 1001);
        let home = root.join("home");
        let grants = root.join("grants");
        std::fs::create_dir(&grants).unwrap();
        std::fs::set_permissions(&grants, std::fs::Permissions::from_mode(0o700)).unwrap();
        let intent = crate::setup::migration_intent::MigrationIntent::capture_with_policy(
            &home,
            &grants,
            [true, false, false],
            vec![],
            crate::setup::migration_handoff::HomePolicy::AccessReadWrite,
        )
        .unwrap();
        let expected = intent.for_media().unwrap();
        let mut copy = expected.clone();
        copy.home_policy = crate::setup::migration_handoff::HomePolicy::Copy;
        assert!(prepared_setup::RetainedHomePlan::new(copy).is_err());
        let mut no_home = expected.clone();
        no_home.categories.home = false;
        assert!(prepared_setup::RetainedHomePlan::new(no_home).is_err());
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let password = "a sufficiently long installation password";
        let media = PreparedMedia::prepare_with_migration(
            &identity,
            None,
            None,
            Some(intent.clone()),
            Zeroizing::new(password.into()),
            "authenticated-shared-files".into(),
            None,
        )
        .unwrap();
        let review = prepared_setup::PreparedSetupReview::open(
            &media.package(),
            password.as_bytes(),
            &root.join("authenticated-review"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(media.home_archive_protocol(), None);
        let retained = review.manifest().retained_home.as_ref().unwrap();
        assert_eq!(retained.as_plan(), &expected);
        let caller = review
            .manifest()
            .retained_caller
            .as_ref()
            .expect("real root helper registered caller");
        caller
            .validate_binding(
                &expected,
                "authenticated-shared-files",
                &identity.public_key(),
            )
            .unwrap();
        assert!(review.manifest().offline_applications.is_none());
        assert!(review.manifest().portable_flatpak.is_none());
        let mut wrong_person = review.manifest().clone();
        wrong_person.identity_public_key = [99; 32];
        assert!(wrong_person.write(&root.join("wrong-person-plan")).is_err());
        let mut wrong_binding = review.manifest().clone();
        wrong_binding.setup_binding.push_str("-other");
        assert!(
            wrong_binding
                .write(&root.join("wrong-binding-plan"))
                .is_err()
        );
        assert!(review.manifest().portable_home.is_none());
        let mut conflicting = review.manifest().clone();
        conflicting.portable_home = Some(prepared_setup::PortableHomePlan {
            schema: 1,
            selection: prepared_setup::PortableHomeSelection::FullHome,
            applications: false,
        });
        assert!(conflicting.write(&root.join("conflicting-plan")).is_err());
        assert_eq!(review.manifest().identity_public_key, identity.public_key());
        assert_eq!(media.projection()["shared_original_files"], true);
        assert!(!media._custody.path().join("setup").exists());
        assert!(!root.join("imported-home").exists());
        assert_eq!(
            std::fs::read(home.join("document")).unwrap(),
            b"original shared document"
        );
        assert_eq!(
            std::fs::read(home.join(".config/private-state")).unwrap(),
            b"original app state"
        );
        assert!(
            prepared_setup::PreparedSetupReview::open(
                &media.package(),
                b"wrong password",
                &root.join("wrong-password"),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        std::fs::rename(&home, root.join("retained-original")).unwrap();
        std::fs::create_dir(&home).unwrap();
        assert!(intent.for_media().is_err());
    }
}

fn read_root_file(path: &Path, maximum: u64) -> Vec<u8> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .unwrap();
    let before = file.metadata().unwrap();
    assert!(
        before.is_file()
            && before.uid() == 0
            && before.nlink() == 1
            && before.mode() & 0o022 == 0
            && before.len() <= maximum
    );
    let mut bytes = Vec::new();
    (&file).take(maximum + 1).read_to_end(&mut bytes).unwrap();
    let after = file.metadata().unwrap();
    assert_eq!(bytes.len() as u64, before.len());
    assert_eq!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.mtime_nsec(),
            before.ctime(),
            before.ctime_nsec()
        ),
        (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec()
        )
    );
    bytes
}
fn chown(path: &Path, uid: u32, gid: u32) {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
}
struct MountedPrivilege {
    path: PathBuf,
}
impl Drop for MountedPrivilege {
    fn drop(&mut self) {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(self.path.as_os_str().as_bytes()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if unsafe { libc::umount2(path.as_ptr(), 0) } == 0 {
                break;
            }
            let error = io::Error::last_os_error();
            assert!(
                error.raw_os_error() == Some(libc::EBUSY) && Instant::now() < deadline,
                "owned private privilege fixture must physically retire"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
struct Actor {
    child: std::process::Child,
    retired: bool,
}
impl Actor {
    fn retire(&mut self) -> std::process::ExitStatus {
        assert!(!self.retired);
        let id = i32::try_from(self.child.id()).unwrap();
        // WNOWAIT retains the exact leader until its process group is retired.
        let killed = unsafe { libc::kill(-id, libc::SIGKILL) };
        assert!(killed == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH));
        let status = self.child.wait().expect("reap exact owned actor");
        self.retired = true;
        status
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        if !self.retired {
            self.retire();
        }
    }
}
/// This selector requires the existing owned-GPT native provider PLUS a genuine
/// matched standalone Imager compiler artifact. Missing inputs are failures.
#[cfg(feature = "native-capture-qualification")]
#[test]
fn full_prepare_shared_files_via_owned_gpt_and_actual_root_helper() {
    run_owned_actor(
        "setup::journey::prepared_media_native::native_retained::actor::shared_files_media_authenticates_original_plan_without_copying_home",
    );
}

/// Re-enter a positive journey test only inside the owned nonroot actor. The
/// parent remains responsible for source custody, genuine elevation and helper
/// artifact authentication; this is not an alternate helper implementation.
pub(in crate::setup) fn enter_owned_journey(selector: &str) -> bool {
    if std::env::var_os("TILO_NATIVE_SHARED_FILES_ACTOR_ROOT").is_some() {
        assert_eq!(rustix::process::getuid().as_raw(), 1000);
        assert_eq!(rustix::process::geteuid().as_raw(), 1000);
        assert_eq!(rustix::process::getgid().as_raw(), 1001);
        return true;
    }
    run_owned_actor(selector);
    false
}

pub(in crate::setup) fn journey_directory() -> tempfile::TempDir {
    let root = std::env::var_os("TILO_NATIVE_SHARED_FILES_ACTOR_ROOT")
        .expect("owned GPT journey actor required");
    tempfile::tempdir_in(root).unwrap()
}

/// Authenticate the actual early preparation artifact, not merely its UI state.
pub(in crate::setup) fn assert_journey_registration(preview: &super::super::InstallerPreview) {
    let media = preview.direct.prepared_belongings.as_ref().unwrap();
    let identity = preview.personal.prepared_handle().unwrap();
    let intent = preview.migration_selection.reviewed_intent().unwrap();
    let expected = intent.for_media().unwrap();
    let review_root = tempfile::tempdir().unwrap();
    let review = prepared_setup::PreparedSetupReview::open(
        &media.package(),
        preview.journey.password.as_bytes(),
        &review_root.path().join("authenticated-review"),
        &AtomicBool::new(false),
    )
    .unwrap();
    let manifest = review.manifest();
    assert_eq!(manifest.identity_public_key, identity.public_key());
    assert_eq!(
        manifest.retained_home.as_ref().unwrap().as_plan(),
        &expected
    );
    manifest
        .retained_caller
        .as_ref()
        .unwrap()
        .validate_binding(&expected, &manifest.setup_binding, &identity.public_key())
        .unwrap();
    assert!(manifest.portable_home.is_none());
    assert_eq!(media.home_archive_protocol(), None);
}

fn run_owned_actor(selector: &str) {
    use std::os::unix::ffi::OsStrExt;
    assert_eq!(rustix::process::getuid().as_raw(), 0);
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let source = PathBuf::from(
        std::env::var_os("TILO_ORIGINAL_SOURCE_GPT_STORE").expect("owned GPT provider required"),
    );
    let marker = source.join(".tilo-original-source-qualification");
    let metadata = std::fs::symlink_metadata(&marker).unwrap();
    assert!(
        metadata.is_file()
            && metadata.uid() == 0
            && metadata.nlink() == 1
            && metadata.mode() & 0o777 == 0o600
    );
    assert_eq!(
        std::fs::read(marker).unwrap(),
        b"disposable-original-source-only"
    );
    let helper = PathBuf::from(
        std::env::var_os("TILO_NATIVE_IMAGER_HELPER")
            .expect("matched standalone helper artifact required"),
    );
    let expected =
        std::env::var("TILO_NATIVE_IMAGER_HELPER_SHA256").expect("actual artifact digest required");
    assert!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let compiler = PathBuf::from(
        std::env::var_os("TILO_NATIVE_IMAGER_HELPER_COMPILER_JSON")
            .expect("canonical actual compiler JSON required"),
    );
    let compiler_expected = std::env::var("TILO_NATIVE_IMAGER_HELPER_COMPILER_SHA256")
        .expect("compiler receipt digest required");
    let compiler_bytes = read_root_file(&compiler, 64 * 1024 * 1024);
    assert_eq!(
        hex::encode(Sha256::digest(&compiler_bytes)),
        compiler_expected
    );
    let mut records = Vec::new();
    for line in compiler_bytes
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: serde_json::Value = serde_json::from_slice(line).unwrap();
        if value["reason"] == "compiler-artifact"
            && value["target"]["name"] == "imager"
            && value["target"]["kind"] == serde_json::json!(["bin"])
        {
            records.push(value);
        }
    }
    let [record] = records.as_slice() else {
        panic!("exact standalone Imager compiler artifact required")
    };
    assert_eq!(
        record["manifest_path"],
        concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")
    );
    assert_eq!(
        record["target"]["src_path"],
        concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs")
    );
    assert_eq!(record["profile"]["test"], false);
    assert!(record["executable"].as_str().unwrap().ends_with("/imager"));
    let helper_bytes = read_root_file(&helper, 1024 * 1024 * 1024);
    assert!(helper_bytes.starts_with(b"\x7fELF"));
    assert_eq!(hex::encode(Sha256::digest(&helper_bytes)), expected);
    let parent = tempfile::Builder::new()
        .permissions(Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let control = parent.path().join("private-privilege");
    std::fs::create_dir(&control).unwrap();
    let point = std::ffi::CString::new(control.as_os_str().as_bytes()).unwrap();
    let filesystem = c"tmpfs";
    let options = c"mode=0755,size=1G";
    assert_eq!(
        unsafe {
            libc::mount(
                filesystem.as_ptr(),
                point.as_ptr(),
                filesystem.as_ptr(),
                libc::MS_NODEV,
                options.as_ptr().cast(),
            )
        },
        0,
        "private namespace tmpfs prerequisite"
    );
    let _mount = MountedPrivilege {
        path: control.clone(),
    };
    // Profile stores fsync readable ancestors. This root-controlled directory
    // contains only fixed public artifact names; actor state stays mode 0700.
    std::fs::set_permissions(parent.path(), Permissions::from_mode(0o755)).unwrap();
    let held = File::open(&control).unwrap();
    let mut facts = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    assert_eq!(
        unsafe { libc::fstatvfs(held.as_raw_fd(), facts.as_mut_ptr()) },
        0
    );
    let flags = unsafe { facts.assume_init() }.f_flag;
    assert_eq!(
        flags & (libc::ST_NOSUID | libc::ST_NOEXEC),
        0,
        "native bridge requires genuine kernel elevation/execution"
    );
    let main = control.join("matched-imager");
    std::fs::write(&main, helper_bytes).unwrap();
    std::fs::set_permissions(&main, Permissions::from_mode(0o555)).unwrap();
    let helper_meta = std::fs::symlink_metadata(&main).unwrap();
    let actor = control.join("captured-actor");
    // Pin the kernel's current executable, not a mutable pathname alias.
    let actor_file = File::open("/proc/self/exe").unwrap();
    let actor_before = actor_file.metadata().unwrap();
    assert!(
        actor_before.is_file()
            && actor_before.len() <= 1024 * 1024 * 1024
            && actor_before.mode() & 0o022 == 0
    );
    let mut actor_bytes = Vec::new();
    (&actor_file)
        .take(1024 * 1024 * 1024 + 1)
        .read_to_end(&mut actor_bytes)
        .unwrap();
    let actor_after = actor_file.metadata().unwrap();
    assert_eq!(
        (
            actor_before.dev(),
            actor_before.ino(),
            actor_before.len(),
            actor_before.ctime(),
            actor_before.ctime_nsec()
        ),
        (
            actor_after.dev(),
            actor_after.ino(),
            actor_after.len(),
            actor_after.ctime(),
            actor_after.ctime_nsec()
        )
    );
    assert_eq!(actor_bytes.len() as u64, actor_before.len());
    std::fs::write(&actor, actor_bytes).unwrap();
    std::fs::set_permissions(&actor, Permissions::from_mode(0o555)).unwrap();
    let tmp = control.join("actor-tmp");
    std::fs::create_dir(&tmp).unwrap();
    chown(&tmp, 1000, 1001);
    std::fs::set_permissions(&tmp, Permissions::from_mode(0o700)).unwrap();
    let tmp_meta = std::fs::metadata(&tmp).unwrap();
    let public = |path: &Path| {
        serde_json::to_string(path.to_str().expect("fixed native ASCII artifact location")).unwrap()
    };
    let digest = hex::decode(expected).unwrap();
    let facts = format!(
        "#define ACTOR_PROGRAM {}\n#define ACTOR_TMP {}\n#define ACTOR_DEV {}ull\n#define ACTOR_INO {}ull\n#define HELPER_PROGRAM {}\n#define HELPER_DEV {}ull\n#define HELPER_INO {}ull\n#define HELPER_SIZE {}ull\nstatic const unsigned char HELPER_SHA256[32]={{{}}};\n",
        public(&actor),
        public(&tmp),
        tmp_meta.dev(),
        tmp_meta.ino(),
        public(&main),
        helper_meta.dev(),
        helper_meta.ino(),
        helper_meta.len(),
        digest
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    std::fs::write(control.join("retained-helper-facts.h"), facts).unwrap();
    let c = control.join("bridge.c");
    std::fs::write(&c, include_str!("retained_helper_bridge.c")).unwrap();
    let executable = control.join("pkexec");
    let compile_stdout = File::create(control.join("compiler.stdout")).unwrap();
    let compile_stderr = File::create(control.join("compiler.stderr")).unwrap();
    let mut compiler = Command::new("/usr/bin/cc");
    compiler
        .arg("-O2")
        .arg("-Wall")
        .arg("-Werror")
        .arg(&c)
        .arg("-lcrypto")
        .arg("-o")
        .arg(&executable)
        .stdin(Stdio::null())
        .stdout(compile_stdout)
        .stderr(compile_stderr);
    let compiler_parent = unsafe { libc::getpid() };
    unsafe {
        compiler.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != compiler_parent
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                || libc::getppid() != compiler_parent
            {
                return Err(io::Error::other("owned compiler parent changed"));
            }
            Ok(())
        });
    }
    let mut compiler = Actor {
        child: compiler.spawn().unwrap(),
        retired: false,
    };
    let compiler_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        for name in ["compiler.stdout", "compiler.stderr"] {
            assert!(
                std::fs::metadata(control.join(name)).unwrap().len() <= 64 * 1024,
                "fixed compiler output exceeded admitted bound"
            );
        }
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    compiler.child.id(),
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        let info = unsafe { info.assume_init() };
        if unsafe { info.si_pid() } != 0 {
            assert_eq!(
                unsafe { info.si_pid() },
                i32::try_from(compiler.child.id()).unwrap()
            );
            assert!(
                compiler.retire().success(),
                "fixed native bridge compiler/OpenSSL prerequisite refused"
            );
            break;
        }
        assert!(
            Instant::now() < compiler_deadline,
            "fixed compiler deadline exceeded"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::set_permissions(&executable, Permissions::from_mode(0o4555)).unwrap();
    let work = tempfile::Builder::new()
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(&source)
        .unwrap();
    chown(work.path(), 1000, 1001);
    let home = work.path().join("home");
    std::fs::create_dir(&home).unwrap();
    chown(&home, 1000, 1001);
    std::fs::set_permissions(&home, Permissions::from_mode(0o700)).unwrap();
    let config = home.join(".config");
    std::fs::create_dir(&config).unwrap();
    chown(&config, 1000, 1001);
    std::fs::set_permissions(&config, Permissions::from_mode(0o700)).unwrap();
    for (path, bytes) in [
        (
            home.join("document"),
            b"original shared document".as_slice(),
        ),
        (
            config.join("private-state"),
            b"original app state".as_slice(),
        ),
    ] {
        std::fs::write(&path, bytes).unwrap();
        chown(&path, 1000, 1001);
        std::fs::set_permissions(path, Permissions::from_mode(0o600)).unwrap();
    }
    let stdout = File::create(control.join("actor.stdout")).unwrap();
    let stderr = File::create(control.join("actor.stderr")).unwrap();
    let mut command = Command::new(&actor);
    command
        .arg("--exact")
        .arg(selector)
        // The positive journeys are ignored outside this harness.
        .arg("--include-ignored")
        .arg("--nocapture")
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", control.display()))
        .env("TMPDIR", &tmp)
        // Full framework journeys retain large App/test frames in debug builds.
        .env("RUST_MIN_STACK", "16777216")
        // The actor's profile durability boundary must be owned and readable;
        // it must never inherit the builder user's real home directory.
        .env("HOME", &tmp)
        .env("TILO_NATIVE_SHARED_FILES_ACTOR_ROOT", work.path())
        .env("TILO_ORIGINAL_SOURCE_GPT_STORE", &source)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr);
    let actor_parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            };
            let groups = [1002, 1003];
            if libc::setgroups(groups.len(), groups.as_ptr()) < 0
                || libc::setgid(1001) < 0
                || libc::setuid(1000) < 0
            {
                return Err(io::Error::last_os_error());
            };
            if libc::getppid() != actor_parent
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0
                || libc::getppid() != actor_parent
            {
                return Err(io::Error::other("owned actor parent changed"));
            };
            Ok(())
        });
    }
    let mut child = Actor {
        child: command.spawn().unwrap(),
        retired: false,
    };
    let deadline = Instant::now() + Duration::from_secs(90);
    let observed_success = loop {
        for name in ["actor.stdout", "actor.stderr"] {
            assert!(
                std::fs::metadata(control.join(name)).unwrap().len() <= 64 * 1024,
                "owned actor output exceeded admitted bound"
            );
        }
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    child.child.id(),
                    info.as_mut_ptr(),
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            },
            0
        );
        let info = unsafe { info.assume_init() };
        if unsafe { info.si_pid() } != 0 {
            assert_eq!(
                unsafe { info.si_pid() },
                i32::try_from(child.child.id()).unwrap()
            );
            break info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0;
        }
        assert!(
            Instant::now() < deadline,
            "actual native producer deadline exceeded"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let status = child.retire();
    assert!(
        observed_success && status.success(),
        "actual native actor {selector} refused: stdout={} stderr={}",
        String::from_utf8_lossy(&std::fs::read(control.join("actor.stdout")).unwrap()),
        String::from_utf8_lossy(&std::fs::read(control.join("actor.stderr")).unwrap())
    );
    let output = std::fs::read(control.join("actor.stdout")).unwrap();
    assert!(output.len() <= 64 * 1024);
    let text = std::str::from_utf8(&output).unwrap();
    assert!(
        text.contains("test result: ok. 1 passed; 0 failed"),
        "exact native subject must execute once"
    );
    assert!(
        std::fs::metadata(control.join("actor.stderr"))
            .unwrap()
            .len()
            <= 64 * 1024
    );
    // Actor's successful prepare returned only after the canonical Controller
    // observed root-helper retirement; no alternate registration record exists.
    drop(child);
    drop(held);
}
