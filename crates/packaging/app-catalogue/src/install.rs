//! Fetching a package the machine does not have yet.
//!
//! Three steps in one blocking call: find the entry in the catalogue,
//! download the `.qpkg` to a staging file, hand the path to
//! `qos-install`.
//!
//! Trust is decided there, not here: `qos-install` recomputes the
//! content-addressed package id and checks the publisher signature
//! fail-closed before extracting, so a tampered download fails at
//! install rather than becoming an installed app. Nothing in this
//! module passes `--allow-self-signed` — an install started by typing
//! a name into search is the last place to relax a signature check.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::CatalogueApp;

/// How long a .qpkg download may take before it's abandoned;
/// overridable via `QOS_CATALOGUE_DOWNLOAD_TIMEOUT` (seconds).
const DEFAULT_DOWNLOAD_TIMEOUT_SECS: u64 = 120;

fn download_timeout() -> Duration {
    std::env::var("QOS_CATALOGUE_DOWNLOAD_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(
            Duration::from_secs(DEFAULT_DOWNLOAD_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

/// Why an install did not happen; every variant renders to something a person can act on — reaches a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// No configured registry offers this id (any more).
    NotOffered { package_id: String },
    /// Nowhere writable to stage the download.
    NoStagingDir,
    /// The download itself failed.
    Download { detail: String },
    /// `qos-install` could not be run at all.
    InstallerMissing { detail: String },
    /// `qos-install` ran and refused; `detail` is its own output — usually the signature or hash complaint.
    Refused { code: i32, detail: String },
}

impl InstallError {
    /// One-line summary for a notification body.
    pub fn user_message(&self) -> String {
        match self {
            InstallError::NotOffered { package_id } => {
                format!("no configured registry offers {package_id}")
            }
            InstallError::NoStagingDir => "there is nowhere writable to download it to".to_string(),
            InstallError::Download { detail } => format!("the download failed: {detail}"),
            InstallError::InstallerMissing { detail } => {
                format!("qos-install could not be run: {detail}")
            }
            InstallError::Refused { code, detail } => {
                let trimmed = detail.trim();
                if trimmed.is_empty() {
                    format!("qos-install refused it (exit {code})")
                } else {
                    format!("qos-install refused it: {trimmed}")
                }
            }
        }
    }
}

impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

/// Where downloaded .qpkgs are staged before install, under the cache dir so a read-only home in a sandbox isn't fatal.
pub fn staging_dir() -> Option<PathBuf> {
    let base = crate::config::cache_dir()?;
    let staged = base.join("staged-qpkg");
    std::fs::create_dir_all(&staged).ok()?;
    Some(staged)
}

/// The installer binary; `QOS_INSTALL_BIN` overrides, matching the store app and compositor launcher resolution.
pub fn qos_install_bin() -> String {
    std::env::var("QOS_INSTALL_BIN").unwrap_or_else(|_| "qos-install".to_string())
}

/// The exact `qos-install` invocation [`install`] runs. Split out so a
/// test can assert on argv without a network or real installer — in
/// particular, that --allow-self-signed is not in it.
pub fn install_argv(staged_qpkg: &Path) -> Vec<String> {
    vec![
        "install".to_string(),
        staged_qpkg.display().to_string(),
        // Replace an older copy of the same id rather than failing — opening
        // an app the user asked for shouldn't stall on a stale install of it.
        "--force".to_string(),
    ]
}

/// How much has to arrive before another progress report. A report
/// per read syscall would be thousands of lines for a few MB; 64 KiB
/// is about a percent of a small .qpkg.
const PROGRESS_STEP_BYTES: u64 = 64 * 1024;

/// Size of one read from the response body.
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// What a caller learns while a package is fetched. `total` is the
/// server's Content-Length when declared; otherwise the caller shows
/// an indeterminate indication instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub received: u64,
    pub total: Option<u64>,
}

/// HTTP-download `url` to `dest`, reporting as the bytes arrive. Blocking.
///
/// `on_progress` fires once when headers land, then at most once per
/// [`PROGRESS_STEP_BYTES`], and once more on the last byte — possible
/// because the body is streamed rather than buffered whole, which
/// also keeps a large package off the heap.
///
/// The bytes are not trusted here — see the module header.
///
/// # Errors
///
/// Returns an error (as a display-ready string) if the HTTP client
/// can't be built, the request/response fails, `dest`'s parent can't
/// be created, the file can't be written, or the server closes the
/// connection with nothing received.
pub fn download_reporting(
    url: &str,
    dest: &Path,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<u64, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("qos-catalogue/0.1")
        .timeout(download_timeout())
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let mut response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| format!("{e}"))?;
    let total = response.content_length();
    on_progress(DownloadProgress { received: 0, total });

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    let file = std::fs::File::create(dest).map_err(|e| format!("write {}: {e}", dest.display()))?;
    let mut writer = std::io::BufWriter::new(file);
    let mut buf = vec![0u8; READ_CHUNK_BYTES];
    let mut received: u64 = 0;
    let mut reported: u64 = 0;
    loop {
        let n = response.read(&mut buf).map_err(|e| format!("{e}"))?;
        if n == 0 {
            break;
        }
        let chunk = buf.get(..n).ok_or_else(|| {
            "internal: read() reported more bytes than the buffer holds".to_string()
        })?;
        writer
            .write_all(chunk)
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
        received = received.saturating_add(n as u64);
        if received.saturating_sub(reported) >= PROGRESS_STEP_BYTES {
            reported = received;
            on_progress(DownloadProgress { received, total });
        }
    }
    writer
        .flush()
        .map_err(|e| format!("write {}: {e}", dest.display()))?;
    drop(writer);

    if received == 0 {
        return Err("the server returned an empty file".to_string());
    }
    on_progress(DownloadProgress { received, total });
    Ok(received)
}

/// [`download_reporting`] for callers with nothing to report to.
///
/// # Errors
///
/// Returns an error under the same conditions as [`download_reporting`].
pub fn download(url: &str, dest: &Path) -> Result<u64, String> {
    download_reporting(url, dest, &mut |_| {})
}

/// Download and install `app`, blocking until `qos-install` exits.
/// The staged file is removed either way: on success the bytes now
/// live in the installed payload tree; on failure a half-written
/// download isn't worth keeping.
///
/// # Errors
///
/// Returns an error under the same conditions as [`install_app_reporting`].
pub fn install_app(app: &CatalogueApp) -> Result<(), InstallError> {
    install_app_reporting(app, &mut |_| {})
}

/// [`install_app`], reporting download progress. Reporting stops at
/// the download — `qos-install` verifies+extracts as one opaque step,
/// and inventing a fraction for it would be a bar that lies.
///
/// # Errors
///
/// Returns an error if there's nowhere writable to stage the
/// download, the download itself fails, `qos-install` can't be run,
/// or it runs and refuses the package.
pub fn install_app_reporting(
    app: &CatalogueApp,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), InstallError> {
    let staging = staging_dir().ok_or(InstallError::NoStagingDir)?;
    // The package id comes from a signed registry entry, but it still
    // becomes a filename here — refuse anything that could escape the
    // staging directory rather than trusting it.
    let safe_id: String = app
        .package_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dest = staging.join(format!("{safe_id}-{}.qpkg", app.version.replace('/', "_")));

    let downloaded = download_reporting(&app.download_url, &dest, on_progress);
    if let Err(detail) = downloaded {
        let _ = std::fs::remove_file(&dest);
        return Err(InstallError::Download { detail });
    }

    let output = Command::new(qos_install_bin())
        .args(install_argv(&dest))
        .output();
    let _ = std::fs::remove_file(&dest);

    match output {
        Err(e) => Err(InstallError::InstallerMissing {
            detail: e.to_string(),
        }),
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let mut detail = String::new();
            detail.push_str(&String::from_utf8_lossy(&out.stderr));
            if detail.trim().is_empty() {
                detail.push_str(&String::from_utf8_lossy(&out.stdout));
            }
            Err(InstallError::Refused {
                code: out.status.code().unwrap_or(1),
                detail: installer_complaint(&detail),
            })
        }
    }
}

/// Resolve `package_id` against the live catalogue, then install it.
///
/// # Errors
///
/// Returns an error under the same conditions as [`install_reporting`].
pub fn install(package_id: &str) -> Result<CatalogueApp, InstallError> {
    install_reporting(package_id, &mut |_| {})
}

/// [`install`], reporting the download's progress as it happens.
///
/// # Errors
///
/// Returns [`InstallError::NotOffered`] if no configured registry
/// offers `package_id`, or the same errors as
/// [`install_app_reporting`] once resolved.
pub fn install_reporting(
    package_id: &str,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<CatalogueApp, InstallError> {
    let listing = crate::available();
    let app = listing
        .apps
        .into_iter()
        .find(|a| a.package_id == package_id)
        .ok_or_else(|| InstallError::NotOffered {
            package_id: package_id.to_string(),
        })?;
    install_app_reporting(&app, on_progress)?;
    Ok(app)
}

/// How much of the installer's complaint survives into a
/// notification; the toast is two lines tall, so more than this
/// clips mid-word, which reads worse than a short sentence.
const DETAIL_MAX_CHARS: usize = 110;

/// The installer's actual complaint, reduced to one readable line.
///
/// Only the **last** non-empty line survives (the lines above are
/// context the card has no room for); a leading `qos-install:` is
/// stripped (the caller already says who refused it); anything still
/// over [`DETAIL_MAX_CHARS`] is elided rather than cut off mid-word.
fn installer_complaint(text: &str) -> String {
    let Some(line) = text.lines().map(str::trim).rfind(|l| !l.is_empty()) else {
        return String::new();
    };
    let line = line
        .strip_prefix("qos-install:")
        .unwrap_or(line)
        .trim()
        .to_string();
    if line.chars().count() <= DETAIL_MAX_CHARS {
        return line;
    }
    let kept: String = line.chars().take(DETAIL_MAX_CHARS).collect();
    format!("{}…", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one argv assertion that is a security property rather than
    /// a formatting detail.
    #[test]
    fn the_install_invocation_never_relaxes_signature_checking() {
        let argv = install_argv(Path::new("/tmp/staged.qpkg"));
        assert_eq!(argv[0], "install");
        assert_eq!(argv[1], "/tmp/staged.qpkg");
        assert!(
            !argv.iter().any(|a| a == "--allow-self-signed"),
            "an install started from a search box must not opt out of \
             the publisher signature check: {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a.starts_with("--require-attestor")),
            "attestor policy is the user's to set, not this path's: {argv:?}"
        );
    }

    #[test]
    fn errors_render_something_a_person_can_act_on() {
        let cases = [
            InstallError::NotOffered {
                package_id: "dev.qos.ghost".into(),
            },
            InstallError::NoStagingDir,
            InstallError::Download {
                detail: "connection refused".into(),
            },
            InstallError::InstallerMissing {
                detail: "No such file or directory".into(),
            },
            InstallError::Refused {
                code: 2,
                detail: "publisher signature did not verify".into(),
            },
        ];
        for c in &cases {
            let msg = c.user_message();
            assert!(!msg.trim().is_empty(), "{c:?} rendered nothing");
            assert!(
                msg.chars()
                    .next()
                    .is_some_and(|ch| ch.is_lowercase() || ch.is_alphanumeric()),
                "{c:?} → {msg:?} should read as a sentence fragment"
            );
        }
        assert!(
            InstallError::Refused {
                code: 2,
                detail: "publisher signature did not verify".into()
            }
            .user_message()
            .contains("publisher signature"),
            "the installer's own reason has to survive to the user"
        );
    }

    /// An empty installer complaint must still say *something* — "qos-install refused it" with a code beats a blank line.
    #[test]
    fn a_silent_refusal_still_reports_the_exit_code() {
        let msg = InstallError::Refused {
            code: 7,
            detail: "   \n  ".into(),
        }
        .user_message();
        assert!(msg.contains('7'), "{msg:?}");
    }

    #[test]
    fn the_complaint_is_the_last_line_and_nothing_else() {
        let text = "reading header\n\nverifying\nqpkg parse: unexpected EOF\n";
        assert_eq!(installer_complaint(text), "qpkg parse: unexpected EOF");
        assert_eq!(installer_complaint(""), "");
        assert_eq!(installer_complaint("   \n\n  "), "");
    }

    /// The rendered toast already says who refused it, so the installer's
    /// own name prefix would stutter — caught by looking at the real notification.
    #[test]
    fn the_installers_own_name_prefix_is_stripped() {
        let msg = InstallError::Refused {
            code: 1,
            detail: installer_complaint("qos-install: qpkg parse: unexpected EOF during skip"),
        }
        .user_message();
        assert_eq!(
            msg,
            "qos-install refused it: qpkg parse: unexpected EOF during skip"
        );
        assert!(
            !msg.contains("qos-install: "),
            "the sentence must not name the installer twice: {msg:?}"
        );
    }

    /// The notification card is two lines tall; a longer complaint is
    /// elided here rather than cut off mid-word by the renderer.
    #[test]
    fn an_overlong_complaint_is_elided_rather_than_clipped() {
        let long = "x".repeat(400);
        let out = installer_complaint(&long);
        assert!(
            out.chars().count() <= DETAIL_MAX_CHARS + 1,
            "elided to {} chars",
            out.chars().count()
        );
        assert!(out.ends_with('…'));
    }
}
