//! Leases exclude across processes and release when their holder exits. A
//! separate test binary: a child spawned while unit tests hold locks would
//! briefly inherit those descriptors and hold the locks with them.

use std::io::{BufRead, Write as _};
use std::path::PathBuf;
use std::rc::Rc;

use app_state::{DocOwner, DocWriter, FsStore};
use automerge::transaction::Transactable as _;
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

/// A second window of one app is a second instance of the same process tree
/// opening the same room document: it joins as a secondary writer under an
/// actor of its own, and the first window keeps the stored author, its own
/// values and its own compatibility with what it wrote.
#[test]
fn a_second_window_writes_beside_the_first_without_taking_its_author() {
    let tmp = TempDir::new().unwrap();
    // Two stores over one profile directory: two instances of the app.
    let first_instance: Rc<dyn DurableStore> = Rc::new(FsStore::new(tmp.path()));
    let second_instance: Rc<dyn DurableStore> = Rc::new(FsStore::new(tmp.path()));
    let key = key();

    let first_owner =
        Rc::new(DocOwner::acquire_shared(Rc::clone(&first_instance), key.clone()).unwrap());
    assert!(
        first_owner.is_primary(),
        "the first window is the stored author"
    );
    let mut first_doc = first_owner.new_document().unwrap();
    first_doc.put(automerge::ROOT, "window", "first").unwrap();
    let first = DocWriter::start_new(&first_owner, &mut first_doc).unwrap();
    first.try_flush().unwrap();
    let author = first_owner.actor().clone();

    let second_owner =
        Rc::new(DocOwner::acquire_shared(Rc::clone(&second_instance), key.clone()).unwrap());
    assert!(
        !second_owner.is_primary(),
        "a second window found the stored author taken and writes as a secondary"
    );
    assert_ne!(second_owner.actor(), &author, "its actor is its own");
    let mut second_doc = second_owner.new_document().unwrap();
    second_doc.put(automerge::ROOT, "window", "second").unwrap();
    let second = DocWriter::start_new(&second_owner, &mut second_doc).unwrap();
    second.try_flush().unwrap();

    // The first window is undisturbed: it holds its own value and still the
    // stored author, and the shared document merged both windows' writes.
    assert_eq!(first_owner.actor(), &author);
    assert!(
        first_owner
            .stored()
            .unwrap()
            .is_some_and(|stored| !stored.bytes.is_empty()),
        "the first window's document is still readable"
    );
    drop((first, second, first_owner, second_owner));

    let reopened: Rc<dyn DurableStore> = Rc::new(FsStore::new(tmp.path()));
    let reopened = DocOwner::acquire_shared(reopened, key).unwrap();
    assert!(reopened.is_primary());
    assert_eq!(
        reopened.actor(),
        &author,
        "the stored author survived a whole second window's session"
    );
}
