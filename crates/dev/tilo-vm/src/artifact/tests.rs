use super::*;

pub(crate) fn fixture(binary: bool) -> (tempfile::TempDir, ArtifactSwap) {
    let dir = tempfile::tempdir().unwrap();
    let mut layers = Vec::new();
    let source = "a".repeat(40);
    for component in COHORT {
        let mut provenance = Provenance::local(component);
        provenance.source_commit = Some(source.clone());
        let inventory_bytes = serde_json::to_vec(&serde_json::json!({
            "provenance": provenance, "files": {"source.rs": "b".repeat(64)}
        }))
        .unwrap();
        let source_digest = crate::build::sha256_hex(&inventory_bytes);
        let inventory_path = format!("{component}.inputs.json");
        std::fs::write(dir.path().join(&inventory_path), &inventory_bytes).unwrap();
        let tree = dir.path().join(format!("{component}.tree"));
        std::fs::create_dir_all(tree.join("usr/bin")).unwrap();
        std::fs::write(tree.join("usr/bin/fixture"), component).unwrap();
        let layer_hash = tilo_vm_protocol::layer_hash::tree_hash(&tree).unwrap();
        let mut stream = Vec::new();
        crate::builderlayer::write_layer(
            &tree,
            Packed {
                component: component.into(),
                target: Some("x86_64-unknown-linux-gnu".into()),
                toolchain: Some("rustc fixture".into()),
                source_digest: Some(source_digest.clone()),
                binary,
                ..Packed::default()
            },
            &[],
            &mut stream,
        )
        .unwrap();
        let stream_path = format!("{component}.stream");
        std::fs::write(dir.path().join(&stream_path), &stream).unwrap();
        layers.push(LayerReceipt {
            component: component.into(),
            profile: if component == "apps+runtime" {
                "thin-app"
            } else {
                "release"
            }
            .into(),
            features: vec!["fixture/dev-overrides".into()],
            target: "x86_64-unknown-linux-gnu".into(),
            toolchain: "rustc fixture".into(),
            source_digest,
            layer_hash,
            stream: FileReceipt {
                path: stream_path,
                sha256: crate::build::sha256_hex(&stream),
                bytes: stream.len() as u64,
            },
            inventory: FileReceipt {
                path: inventory_path,
                sha256: crate::build::sha256_hex(&inventory_bytes),
                bytes: inventory_bytes.len() as u64,
            },
        });
    }
    let receipt = Receipt {
        schema: "tilo.vm.layer-artifacts/v1".into(),
        repository: "tilo/tilo".into(),
        source_commit: source.clone(),
        builder_image: format!(
            "registry.tilo.so/tilo/tilo-builder@sha256:{}",
            "c".repeat(64)
        ),
        run_id: 123,
        run_attempt: 1,
        architecture: "x86_64".into(),
        layers,
    };
    let bytes = serde_json::to_vec(&receipt).unwrap();
    let path = dir.path().join("receipt.json");
    std::fs::write(&path, &bytes).unwrap();
    (
        dir,
        ArtifactSwap {
            component: "apps+runtime".into(),
            slot: Some("review".into()),
            receipt: path,
            source,
            builder_image: receipt.builder_image,
            run_id: 123,
            run_attempt: 1,
            receipt_sha256: crate::build::sha256_hex(&bytes),
        },
    )
}

fn change(request: &mut ArtifactSwap, mutate: impl FnOnce(&mut serde_json::Value)) {
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&request.receipt).unwrap()).unwrap();
    mutate(&mut json);
    let bytes = serde_json::to_vec(&json).unwrap();
    std::fs::write(&request.receipt, &bytes).unwrap();
    request.receipt_sha256 = crate::build::sha256_hex(&bytes);
}

#[test]
fn valid_full_cohort_is_verified_before_any_daemon_or_builder_use() {
    for binary in [false, true] {
        let (_dir, request) = fixture(binary);
        let prepared = prepare(&request).unwrap();
        assert_eq!(prepared.layers.len(), 4);
        for layer in prepared.layers {
            assert!(!layer.packed.reused);
            assert_eq!(
                layer.provenance.source_commit.as_deref(),
                Some(request.source.as_str())
            );
            assert_eq!(layer.provenance.authority, "local");
            assert!(layer.archive.as_file().metadata().unwrap().len() > 0);
        }
    }
}

#[test]
fn wrong_source_builder_run_architecture_or_incomplete_cohort_fails_closed() {
    for case in [
        "source",
        "builder",
        "run",
        "arch",
        "missing",
        "duplicate",
        "inventory",
        "header",
        "cached",
    ] {
        let (_dir, mut request) = fixture(false);
        change(&mut request, |j| match case {
            "source" => j["source_commit"] = "d".repeat(40).into(),
            "builder" => {
                j["builder_image"] = format!(
                    "registry.tilo.so/tilo/tilo-builder@sha256:{}",
                    "d".repeat(64)
                )
                .into();
            }
            "run" => j["run_id"] = 124.into(),
            "arch" => j["architecture"] = "aarch64".into(),
            "missing" => {
                j["layers"].as_array_mut().unwrap().remove(0);
            }
            "duplicate" => j["layers"][0]["component"] = "shell".into(),
            "inventory" => j["layers"][0]["source_digest"] = "e".repeat(64).into(),
            "header" => j["layers"][0]["toolchain"] = "different rustc".into(),
            "cached" => j["layers"][0]["layer_hash"] = "e".repeat(64).into(),
            _ => {}
        });
        assert!(prepare(&request).is_err(), "{case} was admitted");
    }
}

#[test]
fn tampered_missing_escaping_or_trailing_artifacts_are_not_imported() {
    for case in [
        "receipt",
        "payload",
        "inventory",
        "missing",
        "escape",
        "trailing",
        "footer",
        "reused",
    ] {
        let (dir, mut request) = fixture(false);
        let stream = dir.path().join("launcher.stream");
        match case {
            "receipt" => request.receipt_sha256 = "f".repeat(64),
            "payload" => {
                std::fs::write(&stream, "substituted bytes").unwrap();
            }
            "inventory" => {
                std::fs::write(dir.path().join("launcher.inputs.json"), "wrong inventory").unwrap();
            }
            "missing" => {
                std::fs::remove_file(&stream).unwrap();
            }
            "escape" => change(&mut request, |j| {
                j["layers"][0]["stream"]["path"] = "../other.stream".into();
            }),
            "trailing" | "footer" | "reused" => {
                let mut bytes = std::fs::read(&stream).unwrap();
                if case == "trailing" {
                    bytes.extend(b"substituted second layer\n");
                } else if case == "footer" {
                    bytes.truncate(bytes.len() - 90);
                } else {
                    let end = bytes.iter().position(|b| *b == b'\n').unwrap();
                    let mut header: serde_json::Value =
                        serde_json::from_slice(&bytes[crate::builderlayer::BEGIN.len()..end])
                            .unwrap();
                    header["reused"] = true.into();
                    let mut replaced =
                        format!("{}{}\n", crate::builderlayer::BEGIN, header).into_bytes();
                    replaced.extend(&bytes[end + 1..]);
                    bytes = replaced;
                }
                std::fs::write(&stream, &bytes).unwrap();
                change(&mut request, |j| {
                    j["layers"][0]["stream"]["sha256"] = crate::build::sha256_hex(&bytes).into();
                    j["layers"][0]["stream"]["bytes"] = (bytes.len() as u64).into();
                });
            }
            _ => {}
        }
        assert!(prepare(&request).is_err(), "{case} was admitted");
    }
}
