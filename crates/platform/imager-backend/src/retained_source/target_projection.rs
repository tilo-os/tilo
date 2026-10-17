//! Fixed target-credential worker over an already root-admitted FUSE channel.
//! Mounting, source authority and lifecycle remain in the canonical launcher.
use launcher::owned_service::{AdmittedTargetFuse, TargetProjectionMode};
use runtime_fs::{
    directory_connection::DirectoryConnection, directory_fuse::DirectoryFilesystem,
    selected_client::SelectedFileConnection, selected_fuse::SelectedFilesystem,
};
use std::{
    io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            net::UnixListener,
        },
    },
    path::Path,
};
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

pub(super) fn run() -> io::Result<()> {
    // Only the signed shim's independently admitted root-sealed bootstrap can
    // supply this channel. It already installed the full policy and dropped
    // to certified target credentials, with no original source groups.
    let admitted = AdmittedTargetFuse::receive()?;
    let (uid, gid) = admitted.credentials();
    let name = admitted.presentation_name();
    if uid == 0
        || rustix::process::getuid().as_raw() != uid
        || rustix::process::geteuid().as_raw() != uid
        || rustix::process::getgid().as_raw() != gid
        || rustix::process::getegid().as_raw() != gid
    {
        return Err(refused());
    }
    let app = Path::new(launcher::appdir::SANDBOX_APP_DIR);
    let directory = crate::native_root::NativeRoot::retained_service()?;
    let metadata = directory.file().metadata()?;
    if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(refused());
    }
    let path = app.join("provider.sock");
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let published = std::fs::symlink_metadata(&path)?;
    let (mut stream, _) = listener.accept()?;
    // The canonical root controller installs the guarded source relay on this
    // exact stream before waiting for the FUSE handshake-ready observation.
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
    drop(listener);
    runtime_portal::process_binding::complete(&mut stream)?;
    // Only the independently sealed root mode selects the provider protocol.
    // A failed directory bootstrap never falls back to a selected-file route.
    let result = match admitted.projection_mode() {
        TargetProjectionMode::SelectedFile => {
            let source = SelectedFileConnection::connected(stream)?;
            let filesystem = SelectedFilesystem::new(source.clone(), name, uid, gid)?;
            let result = serve(filesystem, admitted, None);
            let retired = source.retire();
            result.and(retired)
        }
        TargetProjectionMode::HomeDirectory | TargetProjectionMode::SelectedParent => {
            // The sealed selected-parent mode uses the authoritative filtered
            // source with the same canonical directory invalidation lifecycle.
            let source = DirectoryConnection::connected(stream)?;
            let filesystem = DirectoryFilesystem::new(source.clone(), uid, gid)?;
            let control = filesystem.invalidation_control();
            let result = serve(filesystem, admitted, Some(control));
            let retired = source.retire();
            result.and(retired)
        }
    };
    directory.validate()?;
    let current = std::fs::symlink_metadata(&path)?;
    if (published.dev(), published.ino()) != (current.dev(), current.ino()) {
        return Err(refused());
    }
    std::fs::remove_file(path)?;
    result
}

fn serve(
    filesystem: impl fuser::Filesystem + 'static,
    admitted: AdmittedTargetFuse,
    invalidations: Option<runtime_fs::directory_fuse::InvalidationControl>,
) -> io::Result<()> {
    let (fuse, ready) = admitted.into_parts();
    let mut config = fuser::Config::default();
    config.n_threads = Some(2);
    // Share only the admitted kernel channel. Never open/clone another FUSE fd.
    config.clone_fd = false;
    let session = fuser::Session::from_fd(filesystem, fuse, fuser::SessionACL::Owner, config)?;
    // Service the actual FUSE channel before a held-view observation can
    // issue fstatfs/getattr. The signed dropped worker retains both lifetimes.
    let background = session.spawn()?;
    let invalidation_worker = invalidations
        .map(|control| control.start(background.notifier()))
        .transpose()?;
    let _observation_result = ready.serve_observations();
    // An interrupted/poisoned READY transcript is not permission to tear down
    // FUSE while root owns a placed clone. Keep both actual background and
    // invalidation lifetimes here; the existing canonical NamespaceOwner stops
    // this exact init only after its private exposure gate permits teardown.
    // Backend authority checks still deny every revoked operation immediately.
    let _lifetimes = (background, invalidation_worker);
    loop {
        std::thread::park_timeout(std::time::Duration::from_secs(1));
    }
}
