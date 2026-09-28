use super::*;
use crate::Store;

const MACHINE: &str = "sealed-host";

fn blob(root: &Path) -> std::path::PathBuf {
    root.join(DIRECTORY).join(secret_vault::VAULT_FILE)
}
fn open(root: &Path, mint: bool) -> Result<HostSecret> {
    HostSecret::load_or_create(root, MACHINE, mint, &LocalFileKey)
}
/// Every encoding of the secret a serializer or logger might produce.
fn encodings(secret: &[u8; 32]) -> Vec<Vec<u8>> {
    vec![
        secret.to_vec(),
        hex::encode(secret).into_bytes(),
        hex::encode_upper(secret).into_bytes(),
        serde_json::to_vec(secret).unwrap(),
        format!("{secret:?}").into_bytes(),
    ]
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn sealed_host_secret_round_trips_and_never_remints() {
    let dir = tempfile::tempdir().unwrap();
    let minted = open(dir.path(), true).unwrap();
    let sealed = std::fs::read(blob(dir.path())).unwrap();
    for encoding in encodings(minted.expose()) {
        assert!(!contains(&sealed, &encoding), "sealed blob holds plaintext");
    }
    for mint in [false, true] {
        let reopened = open(dir.path(), mint).unwrap();
        assert_eq!(reopened.expose(), minted.expose());
        assert_eq!(
            reopened.signing_key().verifying_key(),
            minted.signing_key().verifying_key()
        );
    }
    assert_eq!(std::fs::read(blob(dir.path())).unwrap(), sealed);
    assert_eq!(format!("{minted:?}"), "HostSecret(..)");
}

#[test]
fn missing_sealed_secret_or_key_is_refused_not_replaced() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        open(dir.path(), false).is_err(),
        "no Store may gain a fresh identity"
    );
    assert!(!blob(dir.path()).exists());

    open(dir.path(), true).unwrap();
    let sealed = std::fs::read(blob(dir.path())).unwrap();
    std::fs::remove_file(dir.path().join(DIRECTORY).join(secret_vault::KEY_FILE)).unwrap();
    for mint in [false, true] {
        assert!(open(dir.path(), mint).is_err());
    }
    assert_eq!(std::fs::read(blob(dir.path())).unwrap(), sealed);
    assert!(
        !dir.path()
            .join(DIRECTORY)
            .join(secret_vault::KEY_FILE)
            .exists(),
        "a missing sealing key is never re-minted over an existing blob"
    );
}

#[test]
fn tampered_or_foreign_sealed_secret_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let secret = open(dir.path(), true).unwrap();
    let original = std::fs::read(blob(dir.path())).unwrap();

    let check_refused = |bytes: &[u8]| {
        private_fs::write_atomic(&blob(dir.path()), bytes).unwrap();
        let error = open(dir.path(), false).unwrap_err();
        let rendered = format!("{error:#} {error:?}");
        for encoding in encodings(secret.expose()) {
            assert!(!contains(rendered.as_bytes(), &encoding), "{rendered}");
        }
    };
    // Any modified byte, header or ciphertext.
    for index in [0, 8, original.len() / 2, original.len() - 1] {
        let mut tampered = original.clone();
        tampered[index] ^= 1;
        check_refused(&tampered);
    }
    check_refused(&original[..original.len() - 1]);
    // A blob sealed by another Cloud machine, under its own key.
    let other = tempfile::tempdir().unwrap();
    open(other.path(), true).unwrap();
    check_refused(&std::fs::read(blob(other.path())).unwrap());
    // The same key, but sealed for another machine identity.
    let key = LocalFileKey
        .load_or_create(&dir.path().join(DIRECTORY))
        .unwrap();
    let foreign = envelope::seal(
        &key,
        Context::new(Label::CloudHostSecret, b"another-machine"),
        secret.expose(),
    )
    .unwrap();
    check_refused(&foreign);
    // The same key and machine, but another sealing purpose.
    let other_label = envelope::seal(
        &key,
        Context::new(Label::SecretVault, MACHINE.as_bytes()),
        secret.expose(),
    )
    .unwrap();
    check_refused(&other_label);

    private_fs::write_atomic(&blob(dir.path()), &original).unwrap();
    assert_eq!(open(dir.path(), false).unwrap().expose(), secret.expose());
}

#[test]
fn state_file_holds_no_host_secret_material() {
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(tilo_delivery_protocol::clock::now_ns())
        .unwrap();
    let secret = store.state.host_secret().clone();
    drop(store);
    let state = std::fs::read(dir.path().join("state.json")).unwrap();
    assert!(!contains(&state, b"host_secret"));
    // Neither the checkpoint nor any other Store file outside the sealed
    // directory (journal, lock) carries the secret.
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        for encoding in encodings(secret.expose()) {
            assert!(
                !contains(&bytes, &encoding),
                "{} holds the host secret",
                path.display()
            );
        }
    }
    // The secret still opens from its sealed blob with the Store.
    let store = Store::open(dir.path(), "agent-admission").unwrap();
    assert_eq!(store.state.host_secret().expose(), secret.expose());
}

#[test]
fn state_with_an_inline_host_secret_is_refused() {
    let (dir, store, _) = crate::agent_owner_tests::enrolled();
    let secret = *store.state.host_secret().expose();
    drop(store);
    let path = dir.path().join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["host_secret"] = serde_json::to_value(secret).unwrap();
    let bytes = serde_json::to_vec(&state).unwrap();
    private_fs::write_atomic(&path, &bytes).unwrap();
    assert!(Store::open(dir.path(), "agent-admission").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
