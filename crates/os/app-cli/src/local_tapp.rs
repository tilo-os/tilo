//! `app install <file.tapp | URL>` and `app update <app> --from <file>`:
//! a package that did not come from a repository.
//!
//! The rule is the app host's: a valid signature from a publisher this
//! person trusts installs; a valid signature from a self-signed or unknown
//! publisher asks once, naming the publisher as unverified. With no terminal
//! to ask on, that is refused unless `--yes` is given. `tilo-install` stays
//! the one verifier: its `check` says who signed the file, and the install
//! it then runs verifies the same bytes again.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use app_cli::install::{DownloadProgress, staging_dir, tilo_install_bin};
use clap::Args;

/// How a package file is accepted; shared by `install` and `update --from`.
#[derive(Debug, Args, Clone, Default)]
pub struct FileTrust {
    /// Install a package file from an unverified publisher without asking.
    /// Needed where there is no terminal to ask on.
    #[arg(long, short = 'y', alias = "allow-self-signed")]
    pub yes: bool,
    /// Accept a package file only if this publisher (64 hex characters)
    /// signed it; that publisher then counts as trusted.
    #[arg(long, alias = "expect-publisher", value_name = "KEY")]
    pub require_publisher: Option<String>,
    /// Accept a package file only if its package root is this hash (64 hex
    /// characters).
    #[arg(long, alias = "expect-package-root", value_name = "HASH")]
    pub require_package_root: Option<String>,
    /// Accept a package file only if this trusted attestor vouches for it.
    #[arg(long, alias = "expect-attestor", value_name = "ID")]
    pub require_attestor: Option<String>,
}

impl FileTrust {
    /// The first of these options given, for saying it only applies to a
    /// package file.
    pub fn file_only_flag(&self) -> Option<&'static str> {
        [
            (self.yes, "--yes"),
            (self.require_publisher.is_some(), "--require-publisher"),
            (
                self.require_package_root.is_some(),
                "--require-package-root",
            ),
            (self.require_attestor.is_some(), "--require-attestor"),
        ]
        .into_iter()
        .find_map(|(given, flag)| given.then_some(flag))
    }

    /// The `tilo-install` flags these options are; `accepted` adds
    /// `--allow-self-signed` once the person has accepted an unverified
    /// publisher.
    fn installer_args(&self, accepted: bool) -> Vec<String> {
        let mut args = Vec::new();
        for (flag, value) in [
            ("--require-publisher", &self.require_publisher),
            ("--require-package-root", &self.require_package_root),
            ("--require-attestor", &self.require_attestor),
        ] {
            if let Some(value) = value {
                args.push(flag.to_owned());
                args.push(value.clone());
            }
        }
        if accepted {
            args.push("--allow-self-signed".to_owned());
        }
        args
    }
}

/// Whether `source` names a package file or a link to one rather than an
/// app in a repository: a URL, an existing file, or anything ending in
/// `.tapp` (so a mistyped path says the file is missing, not that no
/// repository has an app by that name).
pub fn is_package_source(source: &str) -> bool {
    is_url(source)
        || Path::new(source)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("tapp"))
        || Path::new(source).is_file()
}

fn is_url(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

/// What `tilo-install check` says about a file.
#[derive(Debug, serde::Deserialize)]
pub struct Verdict {
    pub id: String,
    pub name: String,
    pub version: String,
    pub publisher: String,
    /// `trusted` or `unverified`.
    #[serde(rename = "verdict")]
    pub signer: String,
}

impl Verdict {
    fn verified(&self) -> bool {
        self.signer == "trusted"
    }
}

/// A package file ready to hand to the installer: the person's own file,
/// or a download staged in the cache (removed when this drops).
struct Source {
    path: PathBuf,
    /// The download's staging file, removed with this.
    _downloaded: Option<app_cli::install::StagingFile>,
}

fn fetch(source: &str, on_progress: &mut dyn FnMut(DownloadProgress)) -> Result<Source, String> {
    if !is_url(source) {
        let path = PathBuf::from(source);
        if !path.is_file() {
            return Err(format!("{source}: no such package file"));
        }
        return Ok(Source {
            path,
            _downloaded: None,
        });
    }
    let dir = staging_dir().ok_or("no cache directory to download the package into")?;
    app_cli::install::sweep_abandoned_downloads(&dir);
    let file = app_cli::install::StagingFile::claim(&dir, "download")
        .map_err(|error| format!("{}: {error}", dir.display()))?;
    let staged = Source {
        path: file.path().to_path_buf(),
        _downloaded: Some(file),
    };
    app_cli::install::download_reporting(source, &staged.path, on_progress)
        .map_err(|error| format!("could not download {source}: {error}"))?;
    Ok(staged)
}

/// Ask `tilo-install` who signed `path`.
fn check(path: &Path, trust: &FileTrust) -> Result<Verdict, String> {
    let output = Command::new(tilo_install_bin())
        .arg("check")
        .arg(path)
        .args(trust.installer_args(false))
        .output()
        .map_err(|error| format!("could not run tilo-install: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr
            .trim()
            .trim_start_matches("tilo-install: ")
            .to_owned());
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("tilo-install's verdict was unreadable: {error}"))
}

/// The one sentence naming an unverified publisher.
pub fn unverified_sentence(verdict: &Verdict) -> String {
    format!(
        "{} {} is from an unverified publisher (ed25519:{}): nobody you trust vouches for it",
        verdict.name, verdict.version, verdict.publisher
    )
}

/// Whether an unverified package may go ahead: `--yes`, or a yes typed at
/// the prompt. Without a terminal and without `--yes`, no.
fn accepted(verdict: &Verdict, trust: &FileTrust) -> Result<bool, String> {
    if verdict.verified() {
        return Ok(false);
    }
    if trust.yes {
        return Ok(true);
    }
    let sentence = unverified_sentence(verdict);
    if let Some(attestor) = &trust.require_attestor {
        // An attestor was demanded; asking would quietly drop the demand.
        return Err(format!(
            "{sentence}, and {attestor} did not vouch for it; pass --yes to install it anyway"
        ));
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Err(format!("{sentence}; pass --yes to install it anyway"));
    }
    eprint!("{sentence}.\nInstall it anyway? [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| format!("could not read the answer: {error}"))?;
    if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES") {
        Ok(true)
    } else {
        Err("not installed".to_owned())
    }
}

/// What to do with a checked file.
pub enum Change<'a> {
    Install,
    /// Update the installed app with this id from the file.
    Update(&'a str),
}

/// Install or update from a package file or link, asking about an
/// unverified publisher first.
pub fn apply(
    source: &str,
    change: &Change<'_>,
    trust: &FileTrust,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> ExitCode {
    match prepare(source, change, trust, on_progress) {
        Ok((file, accepted)) => run_installer(&file.path, change, trust, accepted),
        Err(error) => {
            eprintln!("app: {error}");
            ExitCode::FAILURE
        }
    }
}

fn prepare(
    source: &str,
    change: &Change<'_>,
    trust: &FileTrust,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(Source, bool), String> {
    let file = fetch(source, on_progress)?;
    let verdict = check(&file.path, trust)?;
    if let Change::Update(id) = change
        && verdict.id != *id
    {
        return Err(format!("{source} is {}, not {id}", verdict.id));
    }
    let accepted = accepted(&verdict, trust)?;
    Ok((file, accepted))
}

fn run_installer(path: &Path, change: &Change<'_>, trust: &FileTrust, accepted: bool) -> ExitCode {
    let mut command = Command::new(tilo_install_bin());
    match change {
        Change::Install => {
            // Replace an older copy of the same id, as installing from a
            // repository does.
            command.arg("install").arg(path).arg("--force");
        }
        Change::Update(id) => {
            command.args(["update", id, "--from"]).arg(path);
        }
    }
    command.args(trust.installer_args(accepted));
    crate::store_verbs::exit_code(command.status())
}
