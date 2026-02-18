use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use fuser::{MountOption, Config};
use quick_fs_fuse::{QuickFS, MetadataStore, Hydrator, Heuristics};
use tempfile::tempdir;
use tokio::time::timeout;
use iroh::endpoint::Endpoint;
use iroh_blobs::Hash;

#[tokio::test]
async fn test_fuse_ops() {
    // Strict timeout for the entire test
    let test_timeout = Duration::from_secs(15);
    
    let _result = timeout(test_timeout, async {
        // 1. Setup temporary directory for mount
        let mount_dir = tempdir().expect("Failed to create mount temp dir");
        let mount_path = mount_dir.path().to_path_buf();

        // 2. Initialize Iroh Endpoint
        let endpoint = Endpoint::builder().bind().await.expect("Failed to bind endpoint");

        // 3. Initialize QuickFS components
        let metadata = Arc::new(Mutex::new(MetadataStore::new()));
        let hydrator = Arc::new(Hydrator::new(endpoint));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone(), metadata.clone()));

        // Populate with specific test data
        let dummy_hash = Hash::from([0u8; 32]);
        {
            let mut store = metadata.lock().unwrap();
            store.create_file(1, "test_file.txt", 100, dummy_hash);
            let subdir_ino = store.create_dir(1, "subdir");
            store.create_file(subdir_ino, "nested.txt", 200, dummy_hash);
        }

        let fs = QuickFS::new(metadata, hydrator, heuristics);

        // 4. Mount in background
        let options = vec![
            MountOption::RO,
            MountOption::FSName("quickfs_test".to_string()),
        ];
        let mut config = Config::default();
        config.mount_options = options;

        println!("Spawning FUSE mount at {:?}", mount_path);
        // Note: spawn_mount2 is non-blocking
        let _session = fuser::spawn_mount2(fs, &mount_path, &config)
            .expect("Failed to spawn FUSE session");

        // Give it a moment to actually mount
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // 5. Test "ls" (read_dir)
        let entries = std::fs::read_dir(&mount_path)
            .expect("Failed to read_dir")
            .map(|res| res.map(|e| e.file_name().into_string().unwrap()))
            .collect::<Result<Vec<_>, _>>()
            .expect("Error iterating dir");

        println!("Found entries: {:?}", entries);
        assert!(entries.contains(&"test_file.txt".to_string()));
        assert!(entries.contains(&"subdir".to_string()));

        // 6. Test nested ls
        let nested_entries = std::fs::read_dir(mount_path.join("subdir"))
            .expect("Failed to read subdir")
            .map(|res| res.map(|e| e.file_name().into_string().unwrap()))
            .collect::<Result<Vec<_>, _>>()
            .expect("Error iterating subdir");
        
        println!("Found nested entries: {:?}", nested_entries);
        assert!(nested_entries.contains(&"nested.txt".to_string()));

        println!("Integration test basic ops passed!");
    }).await;
}
