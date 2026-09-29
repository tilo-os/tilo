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

    fn start(&self, profile: &Path, folder: &Path) -> WorkspaceFiles {
        let author = Author {
            person: [1; 32],
            device: [2; 32],
        };
        let (store, endpoint) = (Arc::clone(&self.store), self.endpoint.clone());
        WorkspaceFiles::start(profile, folder, store, endpoint, &author).unwrap()
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

    let files = host.start(&profile, &folder);
    std::fs::write(folder.join("todo.txt"), b"water the plants").unwrap();
    wait_for("the folder's file to reach the workspace", || {
        persisted(&profile, "todo.txt")
    });
    drop(files);

    std::fs::remove_file(folder.join("todo.txt")).unwrap();
    let _files = host.start(&profile, &folder);
    wait_for(
        "the removal made while stopped to reach the workspace",
        || !persisted(&profile, "todo.txt"),
    );
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
