use super::*;

fn fixture(entries: &[(&str, u32)]) -> (tempfile::TempDir, LocalLayerImport) {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("launcher.tar");
    let mut builder = tar::Builder::new(File::create(&archive).unwrap());
    for (path, mode) in entries {
        let body = b"qualified producer fixture";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(*mode);
        header.set_cksum();
        builder
            .append_data(&mut header, path, body.as_slice())
            .unwrap();
    }
    builder.finish().unwrap();
    drop(builder);
    let manifest = dir.path().join("components.toml");
    std::fs::write(
        &manifest,
        r#"
[[component]]
name = "launcher"
kind = "userspace"
scope = "session"
packages = ["launcher"]
bins = ["tilo-launcher"]
files = [{ from = "tilo-launcher", to = "/usr/bin/tilo-launcher", mode = "0755" }]
"#,
    )
    .unwrap();
    let mut provenance = Provenance::local("launcher");
    provenance.source_commit = Some("a".repeat(40));
    provenance.cargo_lock_hash = Some("b".repeat(64));
    provenance.toolchain = Some("rustc fixture".into());
    provenance.target = Some("x86_64-unknown-linux-gnu".into());
    let provenance_path = dir.path().join("provenance.json");
    std::fs::write(&provenance_path, serde_json::to_vec(&provenance).unwrap()).unwrap();
    let hash = crate::build::sha256_hex(&std::fs::read(&archive).unwrap());
    (
        dir,
        LocalLayerImport {
            component: "launcher".into(),
            archive,
            archive_sha256: hash,
            provenance: provenance_path,
            manifest,
        },
    )
}

#[test]
fn qualified_local_bytes_are_frozen_before_transport() {
    let (_dir, request) = fixture(&[("usr/bin/tilo-launcher", 0o755)]);
    let expected = std::fs::read(&request.archive).unwrap();
    let mut prepared = prepare(&request).unwrap();
    std::fs::write(&request.archive, b"replaced after validation").unwrap();
    let mut actual = Vec::new();
    prepared.archive.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(prepared.provenance.authority, "local");
    assert_eq!(prepared.provenance.component, "launcher");
    assert_eq!(prepared.hash.len(), 64);
}

#[test]
fn changed_producer_bytes_are_refused() {
    let (_dir, request) = fixture(&[("usr/bin/tilo-launcher", 0o755)]);
    std::fs::write(&request.archive, b"not the qualified archive").unwrap();
    let error = prepare(&request).err().unwrap().to_string();
    assert!(error.contains("producer SHA-256"), "{error}");
}

#[test]
fn extra_duplicate_privileged_and_missing_files_are_refused() {
    for entries in [
        vec![
            ("usr/bin/tilo-launcher", 0o755),
            ("usr/bin/unrelated", 0o755),
        ],
        vec![
            ("usr/bin/tilo-launcher", 0o755),
            ("usr/bin/tilo-launcher", 0o755),
        ],
        vec![("usr/bin/tilo-launcher", 0o4755)],
        vec![("usr/bin/unrelated", 0o755)],
    ] {
        let (_dir, request) = fixture(&entries);
        assert!(
            prepare(&request).is_err(),
            "unexpectedly accepted {entries:?}"
        );
    }
}

#[test]
fn local_inputs_cannot_claim_ci_or_another_component() {
    for (authority, component) in [("ci", "launcher"), ("local", "shell")] {
        let (_dir, request) = fixture(&[("usr/bin/tilo-launcher", 0o755)]);
        let mut provenance: Provenance =
            serde_json::from_slice(&std::fs::read(&request.provenance).unwrap()).unwrap();
        provenance.authority = authority.into();
        provenance.component = component.into();
        std::fs::write(
            &request.provenance,
            serde_json::to_vec(&provenance).unwrap(),
        )
        .unwrap();
        assert!(prepare(&request).is_err());
    }
}

#[test]
fn signed_image_cohorts_are_not_imported_as_local_components() {
    let (_dir, mut request) = fixture(&[("usr/bin/tilo-launcher", 0o755)]);
    request.component = "apps+runtime".into();
    std::fs::write(
        &request.manifest,
        r#"
[[component]]
name = "apps+runtime"
kind = "userspace"
scope = "session"
packages = ["runtime-dylib"]
layer = "direct"
files = [{ from = "/usr/lib/tilo-runtime", to = "/usr/lib/tilo-runtime/", tree = true }]
"#,
    )
    .unwrap();
    let error = prepare(&request).err().unwrap().to_string();
    assert!(error.contains("image-assembled"), "{error}");
}
