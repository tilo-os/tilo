use super::*;
use crate::setup::flatpak_transfer::{
    Installation, Runner,
    portable::{ApplicationImport, NativeOfflineRunner, OfflineRunner},
};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::{
    os::unix::net::UnixStream,
    sync::atomic::{AtomicUsize, Ordering},
};
const APP: &str = "app/org.tilo.Fixture/x86_64/stable";
const RUNTIME: &str = "runtime/org.tilo.FixtureRuntime/x86_64/stable";
struct Observed {
    installs: AtomicUsize,
}
impl Runner for Observed {
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        let value = NativeOfflineRunner.run(home, args)?;
        if args.first().is_some_and(|arg| arg == "install") {
            self.installs.fetch_add(1, Ordering::Relaxed);
        }
        Ok(value)
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        NativeOfflineRunner.verify_repository(home)
    }
}
impl OfflineRunner for Observed {
    fn ostree(&self, repository: &Path, args: &[String]) -> Result<String, String> {
        NativeOfflineRunner.ostree(repository, args)
    }
    fn verify_extra_data(&self, repository: &Path, hash: &str) -> Result<u64, String> {
        NativeOfflineRunner.verify_extra_data(repository, hash)
    }
}
fn authorize() -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 1000
        || rustix::process::geteuid().as_raw() != 1000
        || rustix::process::getgid().as_raw() != 1001
        || rustix::process::getegid().as_raw() != 1001
        || unsafe { libc::getpid() } != 1
        || unsafe { libc::getgroups(0, std::ptr::null_mut()) } != 0
        || std::env::current_exe()? != Path::new(HELPER)
    {
        return Err(invalid());
    }
    // UID transition cleared inherited parent-death signalling. Restore it
    // before receiving root authorization, so root-controller death cannot
    // orphan this namespace init or any target repository writers.
    if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } != 0 {
        return Err(invalid());
    }
    let exe = protected(Path::new(HELPER))?;
    let running = fs::metadata("/proc/self/exe")?;
    let installed = exe.metadata()?;
    if (running.dev(), running.ino()) != (installed.dev(), installed.ino()) {
        return Err(invalid());
    }
    let root = fs::symlink_metadata(ROOT)?;
    if !root.is_dir() || root.uid() != 0 || root.gid() != 0 || root.mode() & 0o7777 != 0o711 {
        return Err(invalid());
    }
    let marker = protected(&Path::new(ROOT).join("owner"))?;
    let mut bytes = Vec::new();
    marker
        .take(MARKER.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes != MARKER {
        return Err(invalid());
    }
    // No target write precedes the supervising root's verification of this
    // exact installed executable, PID1 namespace, UID and direct child route.
    let mut channel = UnixStream::connect(Path::new(ROOT).join("control"))?;
    let credentials = process::credentials(&channel)?;
    if credentials.uid != 0 {
        return Err(invalid());
    }
    channel.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    channel.write_all(b"ready\n")?;
    let mut answer = [0];
    channel.read_exact(&mut answer)?;
    if answer != [1] {
        return Err(invalid());
    }
    Ok(())
}
fn state(home: &Path) -> io::Result<[u8; 32]> {
    let path = home.join(".var/app/org.tilo.Fixture/data/selected-document");
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let m = file.metadata()?;
    if !m.is_file() || m.uid() != 1000 || m.len() != SELECTED_BYTES.len() as u64 {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(SELECTED_BYTES.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes != SELECTED_BYTES {
        return Err(invalid());
    }
    for name in [
        "owner-document",
        "group-only",
        "same-document",
        ".var/app/so.test/data/document",
    ] {
        if fs::symlink_metadata(home.join(name)).is_ok() {
            return Err(invalid());
        }
    }
    Ok(Sha256::digest(&bytes).into())
}
pub(super) fn run(output: &mut impl Write) -> io::Result<()> {
    authorize()?;
    let artifacts = Path::new(ROOT).join("artifacts");
    let expected_file = protected(&artifacts.join("expected.json"))?;
    let mut bytes = Vec::new();
    expected_file
        .take(BOUND as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > BOUND {
        return Err(invalid());
    }
    let expected: crate::setup::flatpak_transfer::portable::Expectation =
        serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    expected.validate()?;
    let receiving = Path::new(ROOT).join("receiving");
    let owner = PinnedDirectory::open_absolute(&receiving)?;
    crate::setup::destination_security::check_private_file(&owner.file)?;
    let cancel = AtomicBool::new(false);
    let setup = crate::setup::prepared_setup::PreparedSetupReview::open(
        &artifacts.join("prepared-setup.sealed"),
        PASSWORD,
        &receiving.join("authenticated"),
        &cancel,
    )?;
    setup.require_completed_export()?;
    if !setup.manifest().completed_applications_only
        || setup.manifest().portable_home.is_some()
        || setup.manifest().completed_home.is_some()
        || setup.manifest().portable_flatpak.as_ref() != Some(&expected)
    {
        return Err(invalid());
    }
    let home = receiving.join("home");
    directory(&home, 0o700, None)?;
    let mut workspace = home.clone();
    for edge in [
        ".local",
        "state",
        "tilo",
        "portable-applications",
        &expected.operation,
    ] {
        workspace.push(edge);
        directory(&workspace, 0o700, None)?;
    }
    let mut apps = ApplicationImport::review(
        &setup,
        &artifacts.join(&expected.filename),
        PASSWORD,
        &workspace.join("imported"),
        &cancel,
    )?;
    let manifest = apps.manifest();
    if manifest.schema != 2
        || manifest.user_state
        || manifest.app_state.len() != 1
        || manifest.app_state[0].app_id != "org.tilo.Fixture"
        || !manifest.app_state[0].data_present
        || manifest.selected.len() != 2
        || manifest.selected.iter().any(|item| {
            item.installation != Installation::User
                || ![APP, RUNTIME].contains(&item.reference.as_str())
        })
        || ![APP, RUNTIME].iter().all(|reference| {
            manifest
                .selected
                .iter()
                .any(|item| item.reference == *reference)
        })
        || manifest.deployments.len() != 2
        || manifest.deployments.iter().any(|item| {
            item.installed.installation != Installation::User
                || ![APP, RUNTIME].contains(&item.installed.reference.as_str())
        })
    {
        return Err(invalid());
    }
    let runner = Observed {
        installs: AtomicUsize::new(0),
    };
    apps.adopt_user_state(&runner, &home, &workspace, &cancel)?;
    apps.restore(&runner, &home, &cancel)?;
    if runner.installs.load(Ordering::Relaxed) != 2 {
        return Err(invalid());
    }
    let before = state(&home)?;
    let mut commits = serde_json::Map::new();
    for deployment in &apps.manifest().deployments {
        let actual = runner
            .run(
                &home,
                &[
                    "info".into(),
                    "--user".into(),
                    "--show-commit".into(),
                    deployment.installed.reference.clone(),
                ],
            )
            .map_err(|_| invalid())?;
        if actual.trim() != deployment.commit {
            return Err(invalid());
        }
        commits.insert(deployment.installed.reference.clone(), actual.trim().into());
    }
    // A real second import invocation exercises authenticated existing extraction
    // and retry, rather than toggling a prepared-state flag on the first object.
    drop(apps);
    let mut retry = ApplicationImport::review(
        &setup,
        &artifacts.join(&expected.filename),
        PASSWORD,
        &workspace.join("imported"),
        &cancel,
    )?;
    retry.adopt_user_state(&runner, &home, &workspace, &cancel)?;
    retry.restore(&runner, &home, &cancel)?;
    if runner.installs.load(Ordering::Relaxed) != 2 || state(&home)? != before {
        return Err(invalid());
    }
    runner.verify_repository(&home).map_err(|_| invalid())?;
    let actual = runner
        .run(
            &home,
            &["list".into(), "--user".into(), "--columns=ref".into()],
        )
        .map_err(|_| invalid())?;
    let mut refs = actual
        .lines()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>();
    refs.sort_unstable();
    if refs != [APP, RUNTIME] {
        return Err(invalid());
    }
    setup.validate()?;
    owner.validate_destination()?;
    // Every repository object/decoder is dropped before PID1 reports completion.
    // The supervising root independently observes its pidfd exit afterwards.
    drop(retry);
    drop(setup);
    drop(owner);
    let result = serde_json::json!({"schema":1,"subset_protocol":2,"deployments":2,"install_commands":2,"commits":commits,"selected_data_sha256":before});
    let bytes = serde_json::to_vec(&result).map_err(|_| invalid())?;
    if bytes.len() > BOUND {
        return Err(invalid());
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    #[test]
    fn ordinary_process_cannot_enter_installed_receiving_factory() {
        assert_ne!(unsafe { libc::getpid() }, 1);
        let mut output = Vec::new();
        assert!(super::run(&mut output).is_err());
        assert!(output.is_empty());
    }
}
