//! Authenticated helper transport and supervision, independent of the UI runtime.
#[cfg(test)]
mod tests;
use crate::media_model::SourceIdentity;
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

pub const WRITE_HELPER_ARG: &str = "--tilo-write-helper";

// Scale in integer space before conversion so even full-width byte counters
// produce bounded progress without lossy integer-to-float casts.
pub(super) fn progress_fraction(bytes: u64, total: u64) -> f64 {
    let scaled = u128::from(bytes.min(total))
        .saturating_mul(u128::from(u32::MAX))
        .checked_div(u128::from(total.max(1)))
        .unwrap_or(0);
    f64::from(u32::try_from(scaled).unwrap_or(u32::MAX)) / f64::from(u32::MAX)
}

/// Serve append-only progress while retaining the privileged operation until
/// its native transaction has completed. Existing output inodes are refused.
///
/// # Errors
/// Returns errors creating the exclusive progress output or serializing, writing
/// or flushing the fallback terminal event. Operation failures are exit codes.
pub fn serve_helper(
    progress_path: &Path,
    cancel_path: &Path,
    run: impl FnOnce(&dyn Fn() -> bool, &mut dyn FnMut(HelperProgress)) -> super::JobResult,
) -> io::Result<i32> {
    let mut progress_file = create_progress_output(progress_path)?;
    let mut terminal_sent = false;
    let result = run(&|| cancel_path.exists(), &mut |progress: HelperProgress| {
        terminal_sent |= matches!(progress.phase, HelperPhase::Success | HelperPhase::Failed);
        if serde_json::to_writer(&mut progress_file, &progress).is_ok() {
            let _ = progress_file.write_all(b"\n");
            let _ = progress_file.flush();
        }
    });
    if !terminal_sent {
        serde_json::to_writer(
            &mut progress_file,
            &HelperProgress {
                phase: HelperPhase::Failed,
                ..HelperProgress::default()
            },
        )?;
        progress_file.write_all(b"\n")?;
        progress_file.flush()?;
    }
    Ok(i32::from(!result.is_success()))
}

/// Digest-checked transport payload, not yet admitted by application policy.
pub struct AuthorizedHelperRequest {
    pub bytes: Vec<u8>,
    pub progress_path: PathBuf,
    pub cancel_path: PathBuf,
    pub caller_uid: Option<u32>,
}

/// Read only the authenticated user's private request and check its signed-in
/// process's digest before deserializing any application-owned request fields.
///
/// # Errors
/// Returns an error for missing or extra helper arguments, invalid private paths,
/// unreadable or oversized requests, failed caller authentication or a digest mismatch.
pub fn read_authorized_helper_request(
    args: &[String],
) -> Result<Option<AuthorizedHelperRequest>, Box<dyn std::error::Error>> {
    let Some(at) = args.iter().position(|arg| arg == WRITE_HELPER_ARG) else {
        return Ok(None);
    };
    let mut arguments = args.iter().skip(at).skip(1);
    let request_path = arguments
        .next()
        .ok_or("write helper request path is missing")?;
    let progress_path = arguments
        .next()
        .ok_or("write helper progress path is missing")?;
    let expected_digest = arguments
        .next()
        .ok_or("write helper request digest is missing")?;
    let cancel_path = arguments
        .next()
        .ok_or("write helper cancellation path is missing")?;
    if arguments.next().is_some() {
        return Err("write helper received unexpected arguments".into());
    }
    validate_helper_paths(
        Path::new(request_path),
        Path::new(progress_path),
        Path::new(cancel_path),
    )?;
    #[cfg(target_os = "linux")]
    let (request_bytes, caller_uid) = read_linux_helper_request(Path::new(request_path))?;
    #[cfg(not(target_os = "linux"))]
    let request_bytes = read_helper_request(Path::new(request_path))?;
    if request_bytes.len() > 1024 * 1024 {
        return Err("write helper request is unexpectedly large".into());
    }
    let actual_digest = hex::encode(Sha256::digest(&request_bytes));
    if expected_digest.len() != 64
        || !expected_digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !actual_digest.eq_ignore_ascii_case(expected_digest)
    {
        return Err("write helper request changed after authorization".into());
    }
    Ok(Some(AuthorizedHelperRequest {
        bytes: request_bytes,
        progress_path: progress_path.into(),
        cancel_path: cancel_path.into(),
        #[cfg(target_os = "linux")]
        caller_uid: Some(caller_uid),
        #[cfg(not(target_os = "linux"))]
        caller_uid: None,
    }))
}

/// Reviewed source identity carried to the elevation boundary.
pub struct ReviewedSource {
    pub path: PathBuf,
    pub size: u64,
    pub identity: Option<SourceIdentity>,
}

/// Wire phases deliberately retain the existing helper JSON representation.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
pub enum HelperPhase {
    #[default]
    Idle,
    Writing,
    Verifying,
    Success,
    Failed,
}

/// Secret-free progress snapshots crossing the native helper boundary.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct HelperProgress {
    pub phase: HelperPhase,
    pub write_frac: f64,
    pub verify_frac: f64,
    pub rate_bps: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// `Start-Process` flattens its `ArgumentList` into a native command line. A
// literal double quote must therefore surround each path so spaces remain in
// the same argv element when the elevated process starts. PowerShell's
// single-quoted `'"'` is exactly one double-quote character; `\"` would be a
// backslash followed by a quote and would not group the path.
#[cfg(any(test, feature = "test-support", target_os = "windows"))]
pub const WINDOWS_ELEVATION_SCRIPT: &str = r#"$p = Start-Process -FilePath $args[0] -Verb RunAs -Wait -PassThru -ArgumentList @('--tilo-write-helper', ('"' + $args[1] + '"'), ('"' + $args[2] + '"'), $args[3], ('"' + $args[4] + '"')); exit $p.ExitCode"#;

#[cfg(not(target_os = "linux"))]
/// Read a bounded helper request on platforms without Linux caller authentication.
///
/// # Errors
/// Returns an error if the request cannot be opened or read.
pub fn read_helper_request(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(target_os = "linux")]
/// Read a bounded request from a directory private to the authenticated caller.
///
/// # Errors
/// Returns an error if `PKEXEC_UID` is missing or invalid, directory or file
/// ownership or permissions are unsafe, or opening, inspecting or reading fails.
pub fn read_linux_helper_request(request_path: &Path) -> io::Result<(Vec<u8>, u32)> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let caller_uid = std::env::var("PKEXEC_UID")
        .map_err(|_| io::Error::other("write helper is missing PKEXEC_UID"))?
        .parse::<u32>()
        .map_err(|_| io::Error::other("write helper received an invalid PKEXEC_UID"))?;
    let directory = request_path
        .parent()
        .ok_or_else(|| io::Error::other("write helper request has no parent directory"))?;
    let directory_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory)?;
    let directory_metadata = directory_file.metadata()?;
    if !directory_metadata.is_dir()
        || directory_metadata.uid() != caller_uid
        || directory_metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "write helper job directory is not private to the authenticated user",
        ));
    }
    let name = c"request.json";
    // SAFETY: the held directory descriptor remains live for this call, the
    // filename is a valid C string, and a successful descriptor is moved into
    // exactly one `File` owner below.
    let request_fd = unsafe {
        libc::openat(
            directory_file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if request_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new owned descriptor and ownership is
    // transferred once to this `File`.
    let mut request_file = unsafe { std::fs::File::from_raw_fd(request_fd) };
    let request_metadata = request_file.metadata()?;
    if !request_metadata.is_file()
        || request_metadata.uid() != caller_uid
        || request_metadata.permissions().mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "write helper request is not owned safely by the authenticated user",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut request_file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    Ok((bytes, caller_uid))
}

/// Validate the fixed filenames within one private helper job directory.
///
/// # Errors
/// Returns an error for invalid path layout, failed request metadata lookup or
/// a request that is not a regular file.
pub fn validate_helper_paths(request: &Path, progress: &Path, cancel: &Path) -> io::Result<()> {
    let request_parent = request
        .parent()
        .ok_or_else(|| io::Error::other("write helper request has no parent directory"))?;
    if progress.parent() != Some(request_parent)
        || cancel.parent() != Some(request_parent)
        || request.file_name().and_then(|name| name.to_str()) != Some("request.json")
        || progress.file_name().and_then(|name| name.to_str()) != Some("progress.jsonl")
        || cancel.file_name().and_then(|name| name.to_str()) != Some("cancel")
        || !request_parent
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("tilo-imager-write-"))
    {
        return Err(io::Error::other(
            "write helper paths are outside its private job directory",
        ));
    }
    let metadata = request.symlink_metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::other(
            "write helper request is not a regular file",
        ));
    }
    Ok(())
}

/// Bind a root helper to authority the authenticated desktop process already
/// possessed. `pkexec` preserves stdin, which the unprivileged process opens
/// on the reviewed image before elevation. The helper therefore never opens a
/// caller-named source path with root authority.
///
/// # Errors
/// Returns an error unless running as root with a reviewed source identity and
/// inherited regular-file stdin matching its size and identity, or if inspection fails.
#[cfg(target_os = "linux")]
pub fn bind_linux_helper_source(source: &ReviewedSource) -> io::Result<std::path::PathBuf> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "write helper must run through pkexec",
        ));
    }
    let expected_identity = source.identity.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "write helper source has no reviewed file identity",
        )
    })?;
    let inherited_source = std::fs::File::open("/proc/self/fd/0")?;
    let metadata = inherited_source.metadata()?;
    if !metadata.is_file()
        || metadata.len() != source.size
        || SourceIdentity::from_file(&inherited_source)? != Some(expected_identity)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inherited source image does not match the reviewed file",
        ));
    }
    Ok("/proc/self/fd/0".into())
}

/// Create a progress output without overwriting an existing inode.
///
/// # Errors
/// Returns an error if exclusive file creation fails, including an existing path.
pub fn create_progress_output(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Launch and supervise one digest-bound, already secret-stripped request.
///
/// # Errors
/// Returns an error for a missing or changed reviewed source identity, source I/O,
/// private job setup, request persistence, executable lookup, command construction,
/// helper launch or progress-channel supervision failures.
pub fn run_privileged_helper(
    source: &ReviewedSource,
    request_bytes: &[u8],
    should_cancel: &impl Fn() -> bool,
    mut on_progress: impl FnMut(HelperProgress),
) -> io::Result<()> {
    let source_file = OpenOptions::new().read(true).open(&source.path)?;
    let source_metadata = source_file.metadata()?;
    let expected_identity = source.identity.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "reviewed source has no stable file identity",
        )
    })?;
    if !source_metadata.is_file()
        || source_metadata.len() != source.size
        || SourceIdentity::from_file(&source_file)? != Some(expected_identity)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source image changed after review",
        ));
    }
    let work = tempfile::Builder::new()
        .prefix("tilo-imager-write-")
        .tempdir()?;
    let request_path = work.path().join("request.json");
    let progress_path = work.path().join("progress.jsonl");
    let cancel_path = work.path().join("cancel");
    let mut request_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request_path)?;
    request_file.write_all(request_bytes)?;
    request_file.flush()?;
    request_file.sync_all()?;
    drop(request_file);
    let request_digest = hex::encode(Sha256::digest(request_bytes));
    let executable = helper_executable()?;
    let mut child = elevated_helper_command(
        &executable,
        &request_path,
        &progress_path,
        &request_digest,
        &cancel_path,
        source_file,
    )?
    .spawn()?;
    relay_helper_progress(
        &mut child,
        &progress_path,
        &cancel_path,
        should_cancel,
        &mut on_progress,
    )
}

/// Locate the helper executable, preferring a valid Linux `AppImage` override.
///
/// # Errors
/// Returns an error if the current executable path cannot be determined when
/// no valid override is available.
pub fn helper_executable() -> io::Result<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("TILO_IMAGER_APPIMAGE") {
        let path = std::path::PathBuf::from(appimage);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
    }
    std::env::current_exe()
}

/// Relay progress and retain child supervision even on channel failure.
///
/// # Errors
/// Returns errors opening or reading the progress channel or querying child
/// status, or if a successful child exits before creating its progress channel.
pub fn relay_helper_progress(
    child: &mut Child,
    progress_path: &Path,
    cancel_path: &Path,
    should_cancel: &impl Fn() -> bool,
    on_progress: &mut impl FnMut(HelperProgress),
) -> io::Result<()> {
    let result = relay_helper_progress_inner(
        child,
        progress_path,
        cancel_path,
        should_cancel,
        on_progress,
    );
    if result.is_err() {
        // Never abandon an elevated raw-device writer merely because its
        // unprivileged progress channel failed. Keeping the private job
        // directory alive and waiting for the child prevents the UI from
        // claiming the operation stopped while destructive I/O may continue.
        let _ = child.wait();
    }
    result
}

fn relay_helper_progress_inner(
    child: &mut Child,
    progress_path: &Path,
    cancel_path: &Path,
    should_cancel: &impl Fn() -> bool,
    on_progress: &mut impl FnMut(HelperProgress),
) -> io::Result<()> {
    let mut cancel_sent = false;
    let file = loop {
        // A transient filesystem error must not detach supervision from a
        // privileged writer. Leave `cancel_sent` false and retry on the next
        // poll; the UI conservatively rejects a racing Success event too.
        let _ = relay_cancellation(should_cancel, cancel_path, &mut cancel_sent);
        match OpenOptions::new().read(true).open(progress_path) {
            Ok(file) => break file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(status) = child.try_wait()? {
                    on_progress(HelperProgress {
                        phase: HelperPhase::Failed,
                        error: Some(if status.success() {
                            "The writer stopped before reporting progress. Please try again.".into()
                        } else {
                            "Authorization was cancelled or the writer could not start. Please try again."
                                .into()
                        }),
                        ..HelperProgress::default()
                    });
                    return if status.success() {
                        Err(io::Error::other(
                            "writer exited without creating its progress channel",
                        ))
                    } else {
                        Ok(())
                    };
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(error) => return Err(error),
        }
    };
    let mut reader = BufReader::new(file);
    let mut terminal = false;
    loop {
        let _ = relay_cancellation(should_cancel, cancel_path, &mut cancel_sent);
        let mut line = String::new();
        while reader.read_line(&mut line)? != 0 {
            if let Ok(progress) = serde_json::from_str::<HelperProgress>(line.trim_end()) {
                terminal |= matches!(progress.phase, HelperPhase::Success | HelperPhase::Failed);
                on_progress(progress);
            }
            line.clear();
        }
        if let Some(status) = child.try_wait()? {
            // Consume bytes written between the last EOF observation and exit.
            while reader.read_line(&mut line)? != 0 {
                if let Ok(progress) = serde_json::from_str::<HelperProgress>(line.trim_end()) {
                    terminal |=
                        matches!(progress.phase, HelperPhase::Success | HelperPhase::Failed);
                    on_progress(progress);
                }
                line.clear();
            }
            // A helper that reported a detailed Failed terminal event exits
            // non-zero by design. Do not overwrite that useful explanation
            // with a second generic event merely because of its exit status.
            if !terminal {
                on_progress(HelperProgress {
                    phase: HelperPhase::Failed,
                    error: Some(if status.success() {
                        "The writer stopped before reporting completion. Please try again.".into()
                    } else {
                        "Authorization was cancelled or the writer stopped unexpectedly. Please try again."
                            .into()
                    }),
                    ..HelperProgress::default()
                });
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(80));
    }
}

/// Publish cancellation at most once after successfully creating its marker.
///
/// # Errors
/// Returns an error if creating the cancellation marker fails for a reason other
/// than an already existing marker.
pub fn relay_cancellation(
    should_cancel: &impl Fn() -> bool,
    cancel_path: &Path,
    sent: &mut bool,
) -> io::Result<()> {
    if should_cancel() && !*sent {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(cancel_path)
        {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        *sent = true;
    }
    Ok(())
}

/// Construct the platform elevation command without launching it.
///
/// # Errors
/// This platform's command construction is infallible; launch and authorization
/// errors are reported when the command is spawned or supervised.
#[cfg(target_os = "linux")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "all platform implementations share one fallible command-builder signature"
)]
pub fn elevated_helper_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
    digest: &str,
    cancel: &Path,
    source: std::fs::File,
) -> io::Result<Command> {
    let mut command = Command::new("pkexec");
    command
        .arg(exe)
        .arg(WRITE_HELPER_ARG)
        .arg(request)
        .arg(progress)
        .arg(digest)
        .arg(cancel)
        .stdin(Stdio::from(source))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

/// Construct the platform elevation command without launching it.
///
/// # Errors
/// This platform's command construction is infallible; launch and authorization
/// errors are reported when the command is spawned or supervised.
#[cfg(target_os = "macos")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "all platform implementations share one fallible command-builder signature"
)]
pub fn elevated_helper_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
    digest: &str,
    cancel: &Path,
    source: std::fs::File,
) -> io::Result<Command> {
    drop(source);
    const SCRIPT: &str = r#"on run argv
set helperCommand to quoted form of item 1 of argv & " --tilo-write-helper " & quoted form of item 2 of argv & " " & quoted form of item 3 of argv & " " & quoted form of item 4 of argv & " " & quoted form of item 5 of argv
do shell script helperCommand with administrator privileges
end run"#;
    let mut command = Command::new("osascript");
    command
        .arg("-e")
        .arg(SCRIPT)
        .arg(exe)
        .arg(request)
        .arg(progress)
        .arg(digest)
        .arg(cancel)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

/// Construct the platform elevation command without launching it.
///
/// # Errors
/// This platform's command construction is infallible; launch and authorization
/// errors are reported when the command is spawned or supervised.
#[cfg(target_os = "windows")]
#[allow(
    clippy::unnecessary_wraps,
    reason = "all platform implementations share one fallible command-builder signature"
)]
pub fn elevated_helper_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
    digest: &str,
    cancel: &Path,
    source: std::fs::File,
) -> io::Result<Command> {
    drop(source);
    // Values are positional PowerShell arguments, never interpolated into the
    // script. ArgumentList receives individually quoted native arguments.
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_ELEVATION_SCRIPT,
        ])
        .arg(exe)
        .arg(request)
        .arg(progress)
        .arg(digest)
        .arg(cancel)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Ok(command)
}

/// Construct an elevation command on other platforms.
///
/// # Errors
/// Always returns an error because raw media writing is unsupported.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub fn elevated_helper_command(
    _exe: &Path,
    _request: &Path,
    _progress: &Path,
    _digest: &str,
    _cancel: &Path,
    _source: std::fs::File,
) -> io::Result<Command> {
    Err(io::Error::other(
        "raw media writing is unsupported on this platform",
    ))
}
