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
files = [{ from = "tilo-launcher", to = "/usr/libexec/tilo/tilo-launcher", mode = "0755" }]
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

tilo_test_env::scenario! {
    fn local_layer_inputs_are_frozen_and_checked;

    phase qualified_local_bytes_are_frozen_before_transport;
    {
        let (_dir, request) = fixture(&[("usr/libexec/tilo/tilo-launcher", 0o755)]);
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

    phase changed_producer_bytes_are_refused;
    {
        let (_dir, request) = fixture(&[("usr/libexec/tilo/tilo-launcher", 0o755)]);
        std::fs::write(&request.archive, b"not the qualified archive").unwrap();
        let error = prepare(&request).err().unwrap().to_string();
        assert!(error.contains("producer SHA-256"), "{error}");
    }

    phase extra_duplicate_privileged_and_missing_files_are_refused;
    {
        for entries in [
            vec![
                ("usr/libexec/tilo/tilo-launcher", 0o755),
                ("usr/bin/unrelated", 0o755),
            ],
            vec![
                ("usr/libexec/tilo/tilo-launcher", 0o755),
                ("usr/libexec/tilo/tilo-launcher", 0o755),
            ],
            vec![("usr/libexec/tilo/tilo-launcher", 0o4755)],
            vec![("usr/bin/unrelated", 0o755)],
        ] {
            let (_dir, request) = fixture(&entries);
            assert!(
                prepare(&request).is_err(),
                "unexpectedly accepted {entries:?}"
            );
        }
    }

    phase a_declared_link_is_taken_and_any_other_link_refused;
    {
        let linked = |target: &str, at: &str| {
            let (dir, mut request) = fixture(&[("usr/libexec/tilo/tilo-launcher", 0o755)]);
            std::fs::write(
                &request.manifest,
                r#"
[[component]]
name = "launcher"
kind = "userspace"
scope = "session"
packages = ["launcher"]
bins = ["tilo-launcher"]
files = [
  { from = "tilo-launcher", to = "/usr/libexec/tilo/tilo-launcher", mode = "0755" },
  { link = "../libexec/tilo/tilo-launcher", to = "/usr/bin/tilo-launcher" },
]
"#,
            )
            .unwrap();
            let mut builder = tar::Builder::new(File::create(&request.archive).unwrap());
            let body = b"qualified producer fixture";
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "usr/libexec/tilo/tilo-launcher", body.as_slice())
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder.append_link(&mut header, at, target).unwrap();
            builder.finish().unwrap();
            drop(builder);
            request.archive_sha256 = crate::build::sha256_hex(&std::fs::read(&request.archive).unwrap());
            (dir, request)
        };
        let (_dir, request) = linked("../libexec/tilo/tilo-launcher", "usr/bin/tilo-launcher");
        prepare(&request).unwrap();
        for (target, at) in [
            ("/etc/shadow", "usr/bin/tilo-launcher"),
            ("../libexec/tilo/tilo-launcher", "usr/bin/unrelated"),
        ] {
            let (_dir, request) = linked(target, at);
            assert!(prepare(&request).is_err(), "unexpectedly accepted {at} -> {target}");
        }
    }

    phase local_inputs_cannot_claim_ci_or_another_component;
    {
        for (authority, component) in [("ci", "launcher"), ("local", "shell")] {
            let (_dir, request) = fixture(&[("usr/libexec/tilo/tilo-launcher", 0o755)]);
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

    phase a_changed_tree_is_recorded_and_still_local;
    {
        let (_dir, request) = fixture(&[("usr/libexec/tilo/tilo-launcher", 0o755)]);
        let read = |request: &LocalLayerImport| -> Provenance {
            serde_json::from_slice(&std::fs::read(&request.provenance).unwrap()).unwrap()
        };
        let mut provenance = read(&request);
        provenance.dirty_tree_hash = Some("c".repeat(64));
        std::fs::write(&request.provenance, serde_json::to_vec(&provenance).unwrap()).unwrap();
        let prepared = prepare(&request).unwrap();
        assert_eq!(prepared.provenance.authority, "local");
        assert!(prepared.provenance.dirty_tree_hash.is_some());

        provenance.dirty_tree_hash = Some("not-a-hash".into());
        std::fs::write(&request.provenance, serde_json::to_vec(&provenance).unwrap()).unwrap();
        assert!(prepare(&request).is_err());
    }

    phase signed_image_cohorts_are_not_imported_as_local_components;
    {
        let (_dir, mut request) = fixture(&[("usr/libexec/tilo/tilo-launcher", 0o755)]);
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
}
