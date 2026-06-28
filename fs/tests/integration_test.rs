use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use fs::namespace::ROOT_INO;
use fs::{ContentStore, Heuristics, Hydrator, Namespace, QuickFS};
use fuser::{Config, MountOption};
use iroh::endpoint::Endpoint;
use tempfile::tempdir;
use tokio::time::timeout;

// Multi-thread runtime: the FUSE read path uses `block_in_place`, which panics
// on a current-thread runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fuse_projects_tree_and_reads_real_content() {
    let test_timeout = Duration::from_secs(20);

    let _ = timeout(test_timeout, async {
        let mount_dir = tempdir().expect("mount temp dir");
        let mount_path = mount_dir.path().to_path_buf();

        let endpoint = Endpoint::bind().await.expect("bind endpoint");

        // Ingest real content into the local store and seed the namespace with it.
        let store = Arc::new(ContentStore::memory());
        let body = b"hello from the collaborative filesystem";
        let hash = store.ingest(Bytes::from_static(body)).await.expect("ingest");

        let namespace = Arc::new(Mutex::new(Namespace::new("test-peer")));
        {
            let mut ns = namespace.lock().unwrap();
            ns.create_file(ROOT_INO, "test_file.txt", Some(&hash), body.len() as u64, 1)
                .unwrap();
            let subdir = ns.create_dir(ROOT_INO, "subdir", 1).unwrap();
            ns.create_file(subdir, "nested.txt", Some(&hash), body.len() as u64, 1)
                .unwrap();
        }

        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), namespace.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics);

        let options = vec![
            MountOption::RO,
            MountOption::FSName("quickfs_test".to_string()),
        ];
        let mut config = Config::default();
        config.mount_options = options;

        let _session =
            fuser::spawn_mount2(fs, &mount_path, &config).expect("spawn FUSE session");
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // Projection: the tree is visible.
        let entries = std::fs::read_dir(&mount_path)
            .expect("read_dir")
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        assert!(entries.contains(&"test_file.txt".to_string()));
        assert!(entries.contains(&"subdir".to_string()));

        let nested = std::fs::read_dir(mount_path.join("subdir"))
            .expect("read subdir")
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        assert!(nested.contains(&"nested.txt".to_string()));

        // Hydration: reading the file returns the real ingested bytes.
        let read_back = std::fs::read(mount_path.join("test_file.txt")).expect("read file");
        assert_eq!(read_back, body, "FUSE read must return the real content");
    })
    .await;
}
