//! Legacy authenticated readiness adapter for the installed Tilo session.

#[cfg(target_os = "linux")]
use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};

/// Read-only confirmation from the live trusted compositor. The UI runs this
/// off-thread; a missing, unauthentic or incomplete session keeps work pending.
#[cfg(target_os = "linux")]
pub fn trusted_identity_complete() -> Result<(), String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or("No Tilo session runtime directory")?;
    let display = std::env::var_os("WAYLAND_DISPLAY")
        .ok_or("Open Add folders inside Tilo after identity setup")?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let parent = executable
        .parent()
        .ok_or("Import executable has no installed directory")?;
    // Nix seals its actual wrapped ELF path at build time. FHS packages keep
    // the root-owned shell ELF beside this installed import executable.
    let trusted_shell = option_env!("QOS_DESKTOP_TRUSTED_SHELL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| parent.join("tilo-shell"));
    authenticated_session_ready(&runtime, &display, &trusted_shell)
}

#[cfg(target_os = "linux")]
fn authenticated_session_ready(
    runtime: &Path,
    display: &std::ffi::OsStr,
    trusted_shell: &Path,
) -> Result<(), String> {
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::fs::MetadataExt;
    let pin =
        super::migration::PinnedDirectory::open_absolute(runtime).map_err(|e| e.to_string())?;
    let metadata = pin.file.metadata().map_err(|e| e.to_string())?;
    // SAFETY: getuid has no arguments or memory contract.
    let uid = unsafe { libc::getuid() };
    if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err("Session runtime directory is not private".into());
    }
    let bytes = display.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 || bytes.contains(&b'/') {
        return Err("Invalid session display".into());
    }
    let wayland = std::fs::symlink_metadata(runtime.join(display)).map_err(|e| e.to_string())?;
    if !wayland.file_type().is_socket() || wayland.uid() != uid {
        return Err("Untrusted session display".into());
    }
    let key = bytes.iter().fold(String::new(), |mut key, byte| {
        let _ = write!(key, "{byte:02x}");
        key
    });
    let socket = runtime.join(format!("qos-import-{key}-{}.sock", wayland.ino()));
    let mut stream = connect_readiness(&socket)
        .map_err(|_| "Waiting for trusted Tilo identity setup to finish".to_string())?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(250)))
        .map_err(|e| e.to_string())?;
    authenticate_session_peer(&stream, trusted_shell, uid)?;
    pin.validate().map_err(|e| e.to_string())?;
    let mut response = [0_u8; 20];
    stream
        .read_exact(&mut response)
        .map_err(|_| "Tilo identity setup is still pending".to_string())?;
    if &response != b"qos-import-ready-v1\n" {
        return Err("Tilo identity setup is still pending".into());
    }
    let current = std::fs::symlink_metadata(runtime.join(display)).map_err(|e| e.to_string())?;
    if !current.file_type().is_socket()
        || current.ino() != wayland.ino()
        || current.dev() != wayland.dev()
        || current.uid() != uid
    {
        return Err("The Tilo session changed; reopen Add folders.".into());
    }
    pin.validate().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn authenticate_session_peer(
    stream: &std::os::unix::net::UnixStream,
    trusted_shell: &Path,
    uid: u32,
) -> Result<(), String> {
    use cap_fs_ext::OpenOptionsFollowExt;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let mut peer = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: socket is live; peer and len are correctly sized writable storage.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            peer.as_mut_ptr().cast(),
            &raw mut len,
        )
    } != 0
        || len as usize != std::mem::size_of::<libc::ucred>()
    {
        return Err("Cannot authenticate Tilo session".into());
    }
    // SAFETY: successful getsockopt initialized ucred.
    let peer = unsafe { peer.assume_init() };
    if peer.uid != uid {
        return Err("Session readiness peer has a different owner".into());
    }
    let mut options = cap_primitives::fs::OpenOptions::new();
    options
        .read(true)
        .follow(cap_primitives::fs::FollowSymlinks::No);
    let trusted_parent = super::migration::PinnedDirectory::open_absolute(
        trusted_shell
            .parent()
            .ok_or("trusted shell has no parent")?,
    )
    .map_err(|e| e.to_string())?;
    let expected = cap_primitives::fs::open(
        &trusted_parent.file,
        Path::new(
            trusted_shell
                .file_name()
                .ok_or("trusted shell has no name")?,
        ),
        &options,
    )
    .map_err(|e| e.to_string())?;
    let actual =
        std::fs::File::open(format!("/proc/{}/exe", peer.pid)).map_err(|e| e.to_string())?;
    let expected = expected.metadata().map_err(|e| e.to_string())?;
    let actual = actual.metadata().map_err(|e| e.to_string())?;
    if !expected.is_file()
        || expected.uid() != 0
        || expected.mode() & 0o022 != 0
        || expected.ino() != actual.ino()
        || expected.dev() != actual.dev()
    {
        return Err("Readiness peer is not the installed trusted Tilo shell".into());
    }
    trusted_parent.validate().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn connect_readiness(path: &Path) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un consists solely of integer fields and a byte array.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(std::io::Error::other("invalid readiness socket path"));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    // A filled or malicious listener backlog must not hang the check thread.
    // Local AF_UNIX connects finish immediately or fail pending; a later
    // explicit check can retry, without granting identity readiness.
    // SAFETY: valid AF_UNIX socket constants; no pointers are passed.
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: this call uniquely adopts the newly created owned descriptor.
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: address is initialized and its correct size is supplied.
    if unsafe {
        libc::connect(
            owned.as_raw_fd(),
            (&raw const address).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let stream = std::os::unix::net::UnixStream::from(owned);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

#[cfg(all(test, target_os = "linux"))]
fn bounded_listener(path: &Path, backlog: libc::c_int) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    };
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un consists solely of integer fields and a byte array.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(std::io::Error::other("invalid readiness socket path"));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    // SAFETY: valid AF_UNIX socket constants; no pointers are passed.
    let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: this call uniquely adopts the newly created owned descriptor.
    let listener = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: address is initialized and its correct size is supplied.
    if unsafe {
        libc::bind(
            listener.as_raw_fd(),
            (&raw const address).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: listener is a bound AF_UNIX stream socket and backlog is fixed by the test.
    if unsafe { libc::listen(listener.as_raw_fd(), backlog) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(listener)
}

#[cfg(not(target_os = "linux"))]
pub fn trusted_identity_complete() -> Result<(), String> {
    Err("Continue the saved Desktop imports after logging into Tilo on Linux.".into())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_invocation_without_shell_and_fake_ready_peer_are_refused() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        use std::os::unix::net::UnixListener;
        let temp = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
        let root = temp.path();
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        let _wayland = UnixListener::bind(root.join("wayland-1")).unwrap();
        let trusted = Path::new("/usr/bin/true");
        assert!(
            authenticated_session_ready(root, std::ffi::OsStr::new("wayland-1"), trusted).is_err()
        );
        let ino = fs::symlink_metadata(root.join("wayland-1")).unwrap().ino();
        let socket = root.join(format!("qos-import-7761796c616e642d31-{ino}.sock"));
        let _fake = UnixListener::bind(socket).unwrap();
        let error = authenticated_session_ready(root, std::ffi::OsStr::new("wayland-1"), trusted)
            .unwrap_err();
        assert!(
            error.contains("not the installed trusted Tilo shell"),
            "{error}"
        );
        assert!(
            authenticated_session_ready(root, std::ffi::OsStr::new("../escape"), trusted).is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readiness_connection_refuses_a_full_backlog_without_hanging() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("gate");
        let _server = bounded_listener(&path, 1).unwrap();
        let mut held = Vec::new();
        let started = std::time::Instant::now();
        loop {
            match connect_readiness(&path) {
                Ok(stream) => held.push(stream),
                Err(_) => break,
            }
            assert!(
                held.len() < 16,
                "explicit one-slot listener backlog was unexpectedly unbounded"
            );
        }
        assert!(
            !held.is_empty(),
            "listener must accept before backlog saturation"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
