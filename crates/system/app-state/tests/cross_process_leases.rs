//! Leases exclude across processes and release when their holder exits. A
//! separate test binary: a child spawned while unit tests hold locks would
//! briefly inherit those descriptors and hold the locks with them.

use std::io::{BufRead, Write as _};
use std::path::PathBuf;

use app_state::FsStore;
use durable_store::{DurableStore, Lease, StorageKey, StoreError};
use tempfile::TempDir;

fn key() -> StorageKey {
    StorageKey::record("so.tilo.test", "room-a", "held")
}

fn take(store: &FsStore, shared: bool) -> Result<(Lease, bool), StoreError> {
    if shared {
        store
            .lease_shared(&key())
            .map(|shared| (shared.lease, shared.primary))
    } else {
        store.lease(&key()).map(|lease| (lease, true))
    }
}

#[test]
fn lease_holder_child() {
    let Some(root) = std::env::var_os("TILO_FS_STORE_TEST_ROOT") else {
        return;
    };
    let store = FsStore::new(PathBuf::from(root));
    let shared = std::env::var_os("TILO_FS_STORE_TEST_SHARED").is_some();
    let _held = take(&store, shared).unwrap();
    println!("LEASE_HELD");
    std::io::stdout().flush().unwrap();
    let mut release = String::new();
    std::io::stdin().read_line(&mut release).unwrap();
}

#[test]
fn leases_exclude_across_processes_and_release_on_exit() {
    for first_shared in [false, true] {
        for second_shared in [false, true] {
            for terminate in [false, true] {
                let tmp = TempDir::new().unwrap();
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command
                    .args(["--exact", "lease_holder_child", "--nocapture"])
                    .env("TILO_FS_STORE_TEST_ROOT", tmp.path())
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped());
                if first_shared {
                    command.env("TILO_FS_STORE_TEST_SHARED", "1");
                }
                let mut child = command.spawn().unwrap();
                let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
                loop {
                    let mut line = String::new();
                    assert!(output.read_line(&mut line).unwrap() > 0, "child must lease");
                    if line.contains("LEASE_HELD") {
                        break;
                    }
                }
                let store = FsStore::new(tmp.path());
                match take(&store, second_shared) {
                    Ok((_, primary)) => {
                        assert!(first_shared && second_shared);
                        assert!(!primary, "the child holds the writer lock");
                    }
                    Err(error) => {
                        assert!(!(first_shared && second_shared), "{error}");
                        assert!(matches!(error, StoreError::Busy(_)), "{error}");
                    }
                }
                if terminate {
                    child.kill().unwrap();
                } else {
                    child.stdin.take().unwrap().write_all(b"release\n").unwrap();
                }
                child.wait().unwrap();
                let (_lease, primary) = take(&store, second_shared).unwrap();
                assert!(primary);
            }
        }
    }
}
