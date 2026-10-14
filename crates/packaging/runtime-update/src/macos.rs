//! The macOS side: reading the running bundle, fetching the release,
//! checking its code signature and restarting the runtime agent. Each step
//! is the system tool that owns it (`plutil`, `curl`, `ditto`, `codesign`,
//! `launchctl`), so nothing here reimplements Apple's checks.

use crate::{Release, release, swap};
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// `Info.plist` key naming the signed release feed; the signature is at the
/// same URL with `.sig` appended. A bundle without it never updates itself.
pub const FEED_KEY: &str = "TiloUpdateFeedURL";
/// `Info.plist` key holding the hex Ed25519 key the feed is signed with.
pub const PUBLIC_KEY_KEY: &str = "TiloUpdatePublicKey";
/// The launchd agent serving the runtime daemon from this bundle.
pub const RUNTIME_AGENT: &str = "so.tilo.runtime";

/// What a check or update found.
#[derive(Debug)]
pub enum Outcome {
    /// This build names no feed or key, so it updates through its installer.
    Disabled,
    UpToDate {
        version: String,
    },
    Available(Release),
    Installed(Release),
}

/// The installed `Tilo.app` this executable runs from.
pub struct Bundle {
    path: PathBuf,
}

impl Bundle {
    /// The bundle containing the running executable (`Contents/MacOS/…`).
    ///
    /// # Errors
    ///
    /// The executable is not inside an app bundle.
    pub fn running() -> io::Result<Self> {
        let executable = std::env::current_exe()?.canonicalize()?;
        let path = executable
            .ancestors()
            .nth(3)
            .filter(|path| path.extension() == Some(OsStr::new("app")))
            .ok_or_else(|| invalid("tilo-runtime-update must run from inside Tilo.app"))?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// A string value of the bundle's `Info.plist`, or `None` when absent.
    ///
    /// # Errors
    ///
    /// `plutil` cannot run.
    pub fn info(&self, key: &str) -> io::Result<Option<String>> {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-"])
            .arg(self.path.join("Contents/Info.plist"))
            .output()?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .filter(|value| !value.is_empty()))
    }

    /// The requirement a replacement must satisfy: this bundle's own
    /// designated requirement. For a Developer ID build that is its
    /// identifier and team; an ad-hoc build's names its exact hash, which no
    /// other bundle satisfies, so ad-hoc builds never update.
    ///
    /// # Errors
    ///
    /// The bundle is unsigned or `codesign` cannot read it.
    pub fn designated_requirement(&self) -> io::Result<String> {
        let output = run(Command::new("/usr/bin/codesign")
            .args(["--display", "--requirements", "-"])
            .arg(&self.path))?;
        crate::requirement::designated(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| invalid("the installed bundle has no designated requirement"))
    }
}

/// Check the feed and, unless `check_only`, install a newer release.
///
/// # Errors
///
/// Any step failing; the installed bundle is then unchanged.
pub fn update(bundle: &Bundle, check_only: bool) -> Result<Outcome, release::Error> {
    let (Some(feed_url), Some(public_key)) = (bundle.info(FEED_KEY)?, bundle.info(PUBLIC_KEY_KEY)?)
    else {
        return Ok(Outcome::Disabled);
    };
    let current = bundle
        .info("CFBundleShortVersionString")?
        .ok_or(release::Error::Malformed("installed bundle version"))?;
    let parent = bundle
        .path
        .parent()
        .ok_or_else(|| invalid("the installed bundle has no parent directory"))?;
    let work = TempDir::create(parent)?;
    let feed = fetch(&feed_url, &work.0.join("feed.json"))?;
    let signature = fetch(&format!("{feed_url}.sig"), &work.0.join("feed.json.sig"))?;
    let release = Release::verify(
        &std::fs::read(feed)?,
        &std::fs::read_to_string(signature)?,
        &public_key,
    )?;
    if !release.is_newer_than(&current) {
        return Ok(Outcome::UpToDate { version: current });
    }
    if check_only {
        return Ok(Outcome::Available(release));
    }
    let archive = fetch(&release.url, &work.0.join("Tilo.zip"))?;
    release.check_archive(&archive)?;
    let unpacked = work.0.join("unpacked");
    run(Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(&archive)
        .arg(&unpacked))?;
    let staged = parent.join(format!(".Tilo.app.update-{}", std::process::id()));
    std::fs::rename(unpacked.join("Tilo.app"), &staged)?;
    let staged = Staged(staged);
    let requirement = bundle.designated_requirement()?;
    run(Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(format!("-R={requirement}"))
        .arg(&staged.0))?;
    swap::replace_bundle(&bundle.path, &staged.0)?;
    restart_agent()?;
    Ok(Outcome::Installed(release))
}

fn fetch(url: &str, to: &Path) -> io::Result<PathBuf> {
    run(Command::new("/usr/bin/curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args([
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-time",
            "900",
        ])
        .arg("--output")
        .arg(to)
        .arg(url))?;
    Ok(to.to_path_buf())
}

/// Restart the agent so the daemon runs the new bundle. Not loaded is fine:
/// the next app launch starts the new daemon on demand.
fn restart_agent() -> io::Result<()> {
    let uid = rustix::process::getuid().as_raw();
    let status = Command::new("/bin/launchctl")
        .args(["kickstart", "-k", &format!("gui/{uid}/{RUNTIME_AGENT}")])
        .stderr(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        eprintln!(
            "tilo-runtime-update: {RUNTIME_AGENT} is not loaded; the next Tilo app starts the new runtime"
        );
    }
    Ok(())
}

fn run(command: &mut Command) -> io::Result<Output> {
    let output = command.output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(io::Error::other(format!(
            "{} failed: {}",
            command.get_program().display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// A private work directory beside the bundle, so the unpacked bundle can be
/// renamed into place without crossing filesystems. Removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn create(parent: &Path) -> io::Result<Self> {
        let path = parent.join(format!(".tilo-update-{}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An unpacked bundle beside the installed one, removed if the update stops
/// before the swap moves it into place.
struct Staged(PathBuf);

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
