use super::*;
use std::time::Instant;

/// The profile's content store and this device's endpoint, which outlive
/// the workspace files as the host's do.
struct Host {
    store: Arc<tilo_fs::ContentStore>,
    endpoint: iroh::Endpoint,
}

impl Host {
    fn new(profile: &Path) -> Self {
        tilo_rt::app().handle().block_on(async {
            Self {
                store: Arc::new(
                    tilo_fs::ContentStore::open(&profile.join("content"))
                        .await
                        .unwrap(),
                ),
                endpoint: iroh::Endpoint::bind(iroh::endpoint::presets::N0)
                    .await
                    .unwrap(),
            }
        })
    }

    fn start(&self, profile: &Path, folder: &Path, projection: Projection) -> WorkspaceFiles {
        let author = Author {
            person: [1; 32],
            device: [2; 32],
        };
        let (store, endpoint) = (Arc::clone(&self.store), self.endpoint.clone());
        WorkspaceFiles::start(profile, folder, store, endpoint, &author, projection).unwrap()
    }
}

/// Whether the workspace the profile persisted has `name` at its root.
fn persisted(profile: &Path, name: &str) -> bool {
    std::fs::read(profile.join(FILES).join(NAMESPACE))
        .ok()
        .and_then(|bytes| Namespace::from_bytes("reader", &bytes).ok())
        .is_some_and(|mut namespace| {
            namespace
                .lookup(tilo_fs::namespace::ROOT_INO, name)
                .is_some()
        })
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_folder_and_the_profile_keep_the_workspace_files_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let folder = folder_in(&dir.path().join("home"), "Home");
    crate::unix::private_directory(&profile).unwrap();
    let host = Host::new(&profile);

    let files = host.start(&profile, &folder, Projection::Folder);
    std::fs::write(folder.join("todo.txt"), b"water the plants").unwrap();
    wait_for("the folder's file to reach the workspace", || {
        persisted(&profile, "todo.txt")
    });
    drop(files);

    std::fs::remove_file(folder.join("todo.txt")).unwrap();
    let _files = host.start(&profile, &folder, Projection::Folder);
    wait_for(
        "the removal made while stopped to reach the workspace",
        || !persisted(&profile, "todo.txt"),
    );
}

#[test]
fn the_workspace_files_are_served_read_only_on_the_profile_socket() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let folder = folder_in(&dir.path().join("home"), "Home");
    crate::unix::private_directory(&profile).unwrap();
    let host = Host::new(&profile);
    let files = host.start(&profile, &folder, Projection::Folder);
    std::fs::create_dir_all(folder.join("notes")).unwrap();
    std::fs::write(folder.join("notes/plan.txt"), b"ship it").unwrap();
    wait_for("the folder's file to reach the workspace", || {
        persisted(&profile, "notes")
    });

    let socket = profile.join(crate::FILES_SOCKET);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let fs = runtime_fs::connect(&socket).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        let listing = loop {
            let listing = fs.list("notes").await.unwrap();
            if !listing.is_empty() || Instant::now() > deadline {
                break listing;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, "plan.txt");
        assert_eq!(listing[0].metadata.kind, runtime_fs::EntryKind::File);
        let plan = fs
            .open("notes/plan.txt", runtime_fs::OpenMode::Read)
            .await
            .unwrap();
        assert_eq!(plan.read_all().await.unwrap(), b"ship it");
        assert!(
            fs.open("notes/plan.txt", runtime_fs::OpenMode::Write)
                .await
                .is_err()
        );
    });
    drop(files);
    assert!(!socket.exists(), "the socket goes with the files");
}

#[test]
fn a_workspace_name_is_a_folder_name_or_replaced() {
    let home = Path::new("/home/ada");
    assert_eq!(folder_in(home, "Home"), home.join("Tilo/Home"));
    for name in ["", "..", ".hidden", "a/b", "nul\0"] {
        assert_eq!(
            folder_in(home, name),
            home.join("Tilo/Workspace"),
            "{name:?}"
        );
    }
}

#[cfg(target_os = "linux")]
mod mounted {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    fn mounted(path: &Path) -> bool {
        let parent = path.parent().unwrap();
        std::fs::metadata(path).unwrap().dev() != std::fs::metadata(parent).unwrap().dev()
    }

    #[test]
    fn fuse_is_used_only_where_this_user_can_mount_it() {
        let dir = tempfile::tempdir().unwrap();
        let dev_fuse = dir.path().join("fuse");
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let path = Some(bin.as_os_str());
        assert!(!mount::available_in(&dev_fuse, path), "no FUSE device");

        std::fs::write(&dev_fuse, b"").unwrap();
        assert!(!mount::available_in(&dev_fuse, path), "no fusermount3");
        let helper = bin.join("fusermount3");
        std::fs::write(&helper, b"").unwrap();
        assert!(
            !mount::available_in(&dev_fuse, path),
            "fusermount3 not executable"
        );
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(mount::available_in(&dev_fuse, path));
        assert!(!mount::available_in(&dev_fuse, None), "no PATH");
    }

    #[test]
    fn the_mounted_workspace_is_the_folder_after_it_is_unmounted() {
        tilo_test_env::require_capability!(
            Fuse,
            Projection::for_host() == Projection::Mount,
            "requires /dev/fuse and fusermount3"
        );
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        let folder = folder_in(&dir.path().join("home"), "Home");
        crate::unix::private_directory(&profile).unwrap();
        let host = Host::new(&profile);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("before.txt"), b"saved before the mount").unwrap();

        let files = host.start(&profile, &folder, Projection::Mount);
        assert!(mounted(&folder), "the workspace is mounted at its folder");
        assert_eq!(
            std::fs::read(folder.join("before.txt")).unwrap(),
            b"saved before the mount",
            "the folder's own files entered the workspace before it was hidden"
        );
        std::fs::write(folder.join("mounted.txt"), b"written through FUSE").unwrap();
        drop(files);

        assert!(!mounted(&folder), "dropping the files unmounts them");
        assert!(persisted(&profile, "mounted.txt"));
        let _files = host.start(&profile, &folder, Projection::Folder);
        wait_for("the mounted write to reach the folder", || {
            std::fs::read(folder.join("mounted.txt")).ok().as_deref()
                == Some(b"written through FUSE".as_slice())
        });
    }
}
