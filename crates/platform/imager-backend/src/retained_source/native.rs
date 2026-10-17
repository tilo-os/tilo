//! Fixed entry in the existing signed worker. Root/native launcher admission
//! projects one selected file; this worker opens nothing outside that projection.
use runtime_fs::{
    FsOwner, FsRight, NativeGrantAuthority,
    selected_file::SelectedFileBackend,
    workspace_server::{
        PendingHandoffRegistry, WorkspaceBackend, serve_workspace_connected_with_handoffs,
    },
};
use std::{
    fs::File,
    io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixListener,
        },
    },
    path::Path,
    sync::Arc,
};

use super::handoff;

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

fn open_source(home: bool) -> io::Result<File> {
    // This is a fixed shim projection, not an argv/env-selected source path.
    let mut options = File::options();
    options.read(true).write(!home).custom_flags(
        libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CLOEXEC
            | if home { libc::O_DIRECTORY } else { 0 },
    );
    let source = if home {
        use std::os::fd::FromRawFd;
        let metadata =
            unsafe { File::from_raw_fd(launcher::owned_service::RETAINED_SOURCE_DATA_FD) };
        let raw = unsafe {
            libc::openat(
                metadata.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe { File::from_raw_fd(raw) }
    } else {
        options.open(launcher::owned_service::RETAINED_IO_SOURCE_PATH)?
    };
    let facts = source.metadata()?;
    if (home && !facts.is_dir()) || (!home && !facts.is_file()) {
        return Err(refused());
    }
    Ok(source)
}

fn source_backend(
    source: File,
    home: bool,
    authority: &NativeGrantAuthority,
) -> io::Result<(runtime_fs::FsSession, Arc<dyn WorkspaceBackend>)> {
    let owner = FsOwner {
        workspace: "retained-source-service".into(),
        app_id: "so.tilo.file-explorer".into(),
    };
    if home {
        home_backend(source, owner, authority)
    } else {
        let grant = authority
            .admit_file(source, owner.clone(), vec![FsRight::Read, FsRight::Write])
            .map_err(io::Error::other)?;
        let session = authority
            .bind(
                owner,
                uuid::Uuid::new_v4(),
                &[],
                std::slice::from_ref(&grant),
            )
            .map_err(io::Error::other)?;
        let backend = SelectedFileBackend::new(session.clone(), grant).map_err(io::Error::other)?;
        Ok((session, Arc::new(backend)))
    }
}

fn home_backend(
    source: File,
    owner: FsOwner,
    authority: &NativeGrantAuthority,
) -> io::Result<(runtime_fs::FsSession, Arc<dyn WorkspaceBackend>)> {
    let grant = authority
        .admit_directory(
            source,
            owner.clone(),
            vec![
                FsRight::Enumerate,
                FsRight::Read,
                FsRight::Write,
                FsRight::Create,
                FsRight::Rename,
            ],
        )
        .map_err(io::Error::other)?;
    let session = authority
        .bind(
            owner.clone(),
            uuid::Uuid::new_v4(),
            std::slice::from_ref(&grant),
            &[],
        )
        .map_err(io::Error::other)?;
    // Only the fixed signed shim can delegate this same-mount descriptor.
    // It is below a root-only guard, never an app-selected private path.
    let removal = unsafe {
        std::os::fd::FromRawFd::from_raw_fd(launcher::owned_service::RETAINED_REMOVAL_DIRECTORY_FD)
    };
    session
        .bind_removal_directory(&grant, removal)
        .map_err(io::Error::other)?;
    let source_owner: File = unsafe {
        std::os::fd::FromRawFd::from_raw_fd(launcher::owned_service::RETAINED_NATIVE_OWNER_FD)
    };
    session
        .bind_retained_source_owner(&grant, source_owner)
        .map_err(io::Error::other)?;
    let verifier_fd = launcher::owned_service::RETAINED_NATIVE_HISTORY_VERIFIER_FD;
    let present = unsafe { libc::fcntl(verifier_fd, libc::F_GETFD) };
    if present >= 0 {
        let verifier: File = unsafe { std::os::fd::FromRawFd::from_raw_fd(verifier_fd) };
        session
            .bind_retained_history_verifier(&grant, verifier)
            .map_err(io::Error::other)?;
    } else if io::Error::last_os_error().raw_os_error() != Some(libc::EBADF) {
        return Err(io::Error::last_os_error());
    }
    let permit_fd = launcher::owned_service::RETAINED_SELECTED_REPLACEMENT_PERMIT_FD;
    let present = unsafe { libc::fcntl(permit_fd, libc::F_GETFD) };
    if present >= 0 {
        let permit: File = unsafe { std::os::fd::FromRawFd::from_raw_fd(permit_fd) };
        // SDK independently checks sealed root custody, exact owner/native
        // locus and current session; generic Home grants stay unchanged.
        session
            .bind_selected_replace_authority(&grant, permit)
            .map_err(io::Error::other)?;
    } else if io::Error::last_os_error().raw_os_error() != Some(libc::EBADF) {
        return Err(io::Error::last_os_error());
    }
    // Root derives this persistent journal beside the protected captures
    // from the actual retained source registration; no source pathname grants it.
    let state: File = unsafe {
        std::os::fd::FromRawFd::from_raw_fd(launcher::owned_service::RETAINED_REMOVAL_JOURNAL_FD)
    };
    let store = runtime_fs::recovery::OperationOwner::open_retained(state, owner, &session, &grant)
        .map_err(io::Error::other)?;
    authority
        .attach_operations(store)
        .map_err(io::Error::other)?;
    let backend = runtime_fs::directory::DirectoryWorkspaceBackend::new(session.clone(), grant)
        .map_err(io::Error::other)?;
    Ok((session, Arc::new(backend)))
}

fn verify_root_peer(stream: &std::os::unix::net::UnixStream) -> io::Result<()> {
    // Only the native root controller gets the source capability. It forwards
    // guarded bytes/handles to its separately authenticated target session.
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut peer).cast(),
            &raw mut length,
        )
    } < 0
        || length as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != 0
    {
        return Err(refused());
    }
    Ok(())
}

pub fn run(home: bool) -> io::Result<()> {
    let uid = rustix::process::getuid().as_raw();
    if uid == 0 || rustix::process::geteuid().as_raw() != uid {
        return Err(refused());
    }
    let app = Path::new(launcher::appdir::SANDBOX_APP_DIR);
    let directory = crate::native_root::NativeRoot::retained_service()?;
    let metadata = directory.file().metadata()?;
    if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(refused());
    }
    let source = open_source(home)?;
    let authority = NativeGrantAuthority::new();
    let (session, backend) = source_backend(source, home, &authority)?;
    directory.validate()?;
    let path = app.join("provider.sock");
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let published = std::fs::symlink_metadata(&path)?;
    let (stream, _) = listener.accept()?;
    verify_root_peer(&stream)?;
    let registry = Arc::new(PendingHandoffRegistry::default());
    // A duplicate of this exact root-connected endpoint is only a local liveness
    // witness. POLLIN means RPC traffic; it must never be confused with death.
    let witness = Arc::new(stream.try_clone()?);
    session
        .attach_lifetime(Arc::new(move || {
            let mut descriptor = libc::pollfd {
                fd: witness.as_raw_fd(),
                events: libc::POLLRDHUP,
                revents: 0,
            };
            let result = unsafe { libc::poll(&raw mut descriptor, 1, 0) };
            result >= 0
                && descriptor.revents
                    & (libc::POLLRDHUP | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)
                    == 0
        }))
        .map_err(io::Error::other)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut handoffs =
        handoff::Handoffs::start(listener, registry.clone(), runtime.handle().clone())?;
    let server = serve_workspace_connected_with_handoffs(
        stream,
        backend,
        runtime.handle().clone(),
        Some(registry),
    )?;
    let result = server.wait();
    // Preserve source scope until every callback has really returned. Runtime
    // shutdown waits for native I/O; there is no abandoned timeout success.
    session.close();
    let handoff_result = handoffs.retire();
    drop(runtime);
    // The native mutation queue is owned by this authority. Its Drop joins
    // queued/active jobs after the closed session rejects further effects.
    drop(authority);
    directory.validate()?;
    let current = std::fs::symlink_metadata(&path)?;
    if (published.dev(), published.ino()) != (current.dev(), current.ino()) {
        return Err(refused());
    }
    std::fs::remove_file(path)?;
    result.and(handoff_result)
}
