//! The native CLI is the subject.
#![cfg(target_os = "linux")]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use tilo_test_env::SpawnUnderEnvLock as _;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn native() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-installer-prepared-setup-extract")
}
fn run(args: &[&str], input: &[u8]) -> TestResult<Output> {
    let mut child = Command::new(native())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()?;
    child
        .stdin
        .take()
        .ok_or("missing child stdin")?
        .write_all(input)?;
    Ok(child.wait_with_output()?)
}
fn private(path: &Path) -> TestResult {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn private_directories(paths: &[&Path]) -> TestResult {
    for path in paths {
        private(path)?;
    }
    Ok(())
}
fn write(path: &Path, data: &[u8]) -> TestResult {
    fs::write(path, data)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
fn sealed() -> Vec<u8> {
    [b"TILOMIG1".as_slice(), &[42; 72]].concat()
}
fn receipt(data: &[u8]) -> Value {
    json!({"schema":1,"sealed_bytes":data.len(),"ciphertext_sha256":hex::encode(Sha256::digest(data))})
}
fn media(root: &Path) -> TestResult {
    let child = root.join(".tilo-installer");
    private(&child)?;
    let data = sealed();
    write(&child.join("prepared-setup.sealed"), &data)?;
    write(
        &child.join("receipt.json"),
        &serde_json::to_vec(&receipt(&data))?,
    )?;
    Ok(())
}
fn expectation(kind: &str, data: &[u8]) -> TestResult<Value> {
    use imager_backend::setup::{
        browser_transfer, flatpak_transfer::portable as flatpak_portable, portable_home_media,
        portable_transfer::Limits,
    };

    let op = "12345678-1234-4234-8234-123456789abc".to_owned();
    let limits = Limits {
        plaintext_bytes: 1024,
        entries: 10,
        metadata_bytes: 1024,
    };
    let ciphertext_sha256: [u8; 32] = Sha256::digest(data).into();
    let value = match kind {
        "applications" => serde_json::to_value(flatpak_portable::Expectation {
            filename: flatpak_portable::Expectation::filename(&op)?,
            operation: op,
            setup_binding: "example setup".into(),
            sealed_bytes: data.len() as u64,
            plaintext_bytes: 10,
            entries: 1,
            ciphertext_sha256,
            limits,
            kind: flatpak_portable::Applications {
                consumer_protocol: 2,
                manifest_sha256: [3; 32],
            },
        })?,
        "browser" => serde_json::to_value(browser_transfer::Expectation {
            filename: browser_transfer::Expectation::filename(&op)?,
            operation: op,
            setup_binding: "example setup".into(),
            sealed_bytes: data.len() as u64,
            plaintext_bytes: 10,
            entries: 1,
            ciphertext_sha256,
            limits,
            kind: browser_transfer::Browser {
                plan_sha256: [3; 32],
            },
        })?,
        _ => serde_json::to_value(portable_home_media::Expectation {
            filename: portable_home_media::Expectation::filename(&op)?,
            operation: op,
            setup_binding: "example setup".into(),
            sealed_bytes: data.len() as u64,
            plaintext_bytes: 10,
            entries: 1,
            ciphertext_sha256,
            limits,
            kind: portable_home_media::Home { runtime_sockets: 0 },
        })?,
    };
    Ok(value)
}
fn flat_expectation(kind: &str, data: &[u8]) -> TestResult<Value> {
    let base = expectation(kind, data)?;
    let field = |key| base.get(key).cloned().ok_or("missing expectation field");
    let purpose = match kind {
        "applications" => "flatpak_applications",
        "browser" => "browser_profiles",
        _ => "home",
    };
    let logical = match kind {
        "applications" => "manifest_sha256",
        "browser" => "plan_sha256",
        _ => "runtime_sockets",
    };
    let mut flat = json!({
        "schema": 1,
        "operation": field("operation")?,
        "purpose": purpose,
        "filename": field("filename")?,
        "setup_binding": field("setup_binding")?,
        "sealed_bytes": field("sealed_bytes")?,
        "plaintext_bytes": field("plaintext_bytes")?,
        "entries": field("entries")?,
        "ciphertext_sha256": field("ciphertext_sha256")?,
        "limits": field("limits")?,
    });
    let value = if kind == "home" {
        json!(0)
    } else {
        json!([3; 32].to_vec())
    };
    flat.as_object_mut()
        .ok_or("flat expectation must be an object")?
        .insert(logical.into(), value);
    Ok(flat)
}
fn selected_payload_with(root: &Path, kind: &str, data: &[u8], expected: &Value) -> TestResult {
    let mut selection = json!({"schema":1,"prepared_setup":receipt(data)});
    selection
        .as_object_mut()
        .ok_or("selection must be an object")?
        .insert(kind.into(), expected.clone());
    write(
        &root.join(format!(".tilo-installer/{kind}-selection.json")),
        &serde_json::to_vec(&selection)?,
    )?;
    write(
        &root.join(format!(
            "{}.receipt.json",
            expected
                .get("operation")
                .ok_or("missing operation")?
                .as_str()
                .ok_or("non-string field")?
        )),
        &serde_json::to_vec(expected)?,
    )?;
    write(
        &root.join(
            expected
                .get("filename")
                .ok_or("missing filename")?
                .as_str()
                .ok_or("non-string field")?,
        ),
        data,
    )?;
    Ok(())
}
fn selected_payload(root: &Path, kind: &str) -> TestResult<Value> {
    let data = sealed();
    let e = expectation(kind, &data)?;
    selected_payload_with(root, kind, &data, &e)?;
    Ok(e)
}
fn extract(root: &Path, out: &Path) -> TestResult<Output> {
    run(
        &[
            "--source-root",
            root.to_str().ok_or("non-UTF-8 path")?,
            "--out-dir",
            out.to_str().ok_or("non-UTF-8 path")?,
        ],
        &[],
    )
}
#[test]
fn native_extraction_preflight_retention_and_retry() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("media");
    let out = temp.path().join("out");
    let target = temp.path().join("var");
    private(&source).unwrap();
    private(&out).unwrap();
    private(&target).unwrap();
    media(&source).unwrap();
    assert!(extract(&source, &out).unwrap().status.success());
    assert_eq!(
        fs::read(out.join("prepared-setup.sealed")).unwrap(),
        sealed()
    );
    for op in ["preflight-profile", "preflight-home"] {
        assert!(
            run(&[op, "--source-dir", out.to_str().unwrap()], &[])
                .unwrap()
                .status
                .success()
        );
    }
    for _ in 0..2 {
        assert!(
            run(
                &[
                    "retain",
                    "--source-dir",
                    out.to_str().unwrap(),
                    "--target-var",
                    target.to_str().unwrap()
                ],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
    }
    assert_eq!(
        fs::read(target.join("lib/tilo/prepared-setup/prepared-setup.sealed")).unwrap(),
        sealed()
    );
    write(
        &out.join("prepared-setup.sealed"),
        &[b"TILOMIG1".as_slice(), &[43; 72]].concat(),
    )
    .unwrap();
    assert!(
        !run(
            &[
                "retain",
                "--source-dir",
                out.to_str().unwrap(),
                "--target-var",
                target.to_str().unwrap()
            ],
            &[]
        )
        .unwrap()
        .status
        .success()
    );
}
#[test]
fn native_payload_preflight_capacity_and_retention_all_kinds() {
    for kind in ["home", "applications", "browser"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("media");
        let out = temp.path().join("out");
        let target = temp.path().join("var");
        private_directories(&[&root, &out, &target]).unwrap();
        media(&root).unwrap();
        let e = selected_payload(&root, kind).unwrap();
        assert!(extract(&root, &out).unwrap().status.success(), "{kind}");
        assert!(
            run(
                &["preflight-home", "--source-dir", out.to_str().unwrap()],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
        let bytes = run(
            &["home-required-bytes", "--source-dir", out.to_str().unwrap()],
            &[],
        )
        .unwrap();
        assert!(bytes.status.success());
        assert_eq!(
            String::from_utf8(bytes.stdout)
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap(),
            80 + if kind == "applications" { 20 } else { 10 } + 1024 + 8192 + 16 * 1024 * 1024
        );
        assert!(
            !run(
                &[
                    "check-home-capacity",
                    "--source-dir",
                    out.to_str().unwrap(),
                    "--root-bytes",
                    "1024"
                ],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
        assert!(
            run(
                &[
                    "check-home-capacity",
                    "--source-dir",
                    out.to_str().unwrap(),
                    "--root-bytes",
                    "68719476736"
                ],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
        for _ in 0..2 {
            assert!(
                run(
                    &[
                        "retain-home",
                        "--source-dir",
                        out.to_str().unwrap(),
                        "--target-var",
                        target.to_str().unwrap()
                    ],
                    &[]
                )
                .unwrap()
                .status
                .success()
            );
        }
        let expected = target.join(format!("lib/tilo/prepared-{kind}/expectation.json"));
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(expected).unwrap()).unwrap(),
            e
        );
        write(
            &root.join(e["filename"].as_str().unwrap()),
            &[b"TILOMIG1".as_slice(), &[43; 72]].concat(),
        )
        .unwrap();
        assert!(
            !run(
                &["preflight-home", "--source-dir", out.to_str().unwrap()],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
    }
}
tilo_test_env::scenario! {
    fn the_native_extractor_refuses_mutated_or_unsafe_input;

    phase closed_receipt_mutations_are_refused_and_redacted;
    {
        let base = receipt(&sealed());
        let mut mutations = vec![Value::Null, json!(true), json!([])];
        for key in ["schema", "sealed_bytes", "ciphertext_sha256"] {
            let mut v = base.clone();
            v.as_object_mut().unwrap().remove(key);
            mutations.push(v);
            for bad in [
                Value::Null,
                json!(true),
                json!(-1),
                json!(1.0),
                json!("example-private-credential"),
            ] {
                let mut v = base.clone();
                v[key] = bad;
                mutations.push(v);
            }
        }
        let mut extra = base;
        extra["unknown"] = json!("example-private-credential");
        mutations.push(extra);
        for value in mutations {
            let temp = tempfile::tempdir().unwrap();
            let native = temp.path().join("native");
            private(&native).unwrap();
            write(&native.join("prepared-setup.sealed"), &sealed()).unwrap();
            let text = serde_json::to_string(&value).unwrap();
            let n = run(
                &[
                    "preflight",
                    "--source-dir",
                    native.to_str().unwrap(),
                    "--receipt",
                    &text,
                ],
                &[],
            )
            .unwrap();
            assert!(!n.status.success());
            assert!(!String::from_utf8_lossy(&n.stderr).contains("example-private-credential"));
            assert!(!native.join("receipt.json").exists());
        }
    }

    phase malformed_payload_metadata_is_refused;
    {
        for kind in ["home", "applications", "browser"] {
            let base = expectation(kind, &sealed()).unwrap();
            let mut mutations = vec![Value::Null, json!(true), json!([])];
            for key in base.as_object().unwrap().keys() {
                let mut v = base.clone();
                v.as_object_mut().unwrap().remove(key);
                mutations.push(v);
                let mut v = base.clone();
                v[key] = json!(true);
                mutations.push(v);
            }
            let mut extra = base.clone();
            extra["unreviewed_grant"] = json!("/etc");
            mutations.push(extra);
            let mut nested_extra = base.clone();
            nested_extra["kind"]["unreviewed_grant"] = json!("/etc");
            mutations.push(nested_extra);
            let flat = flat_expectation(kind, &sealed()).unwrap();
            let mut flat_extra = flat.clone();
            flat_extra["unreviewed_grant"] = json!("/etc");
            mutations.push(flat_extra);
            let mut flat_bad_schema = flat.clone();
            flat_bad_schema["schema"] = json!(2);
            mutations.push(flat_bad_schema);
            let mut flat_bad_purpose = flat.clone();
            flat_bad_purpose["purpose"] = json!("unexpected");
            mutations.push(flat_bad_purpose);
            if kind == "applications" {
                for protocol in [1, 2] {
                    let mut with_protocol = flat.clone();
                    with_protocol["consumer_protocol"] = json!(protocol);
                    mutations.push(with_protocol);
                }
            }
            for e in mutations {
                let temp = tempfile::tempdir().unwrap();
                let root = temp.path().join("media");
                let native = temp.path().join("native");
                private(&root).unwrap();
                private(&native).unwrap();
                media(&root).unwrap();
                let mut selection = json!({"schema":1,"prepared_setup":receipt(&sealed())});
                selection[kind] = e;
                write(
                    &root.join(format!(".tilo-installer/{kind}-selection.json")),
                    &serde_json::to_vec(&selection).unwrap(),
                )
                .unwrap();
                let n = extract(&root, &native).unwrap();
                assert!(!n.status.success(), "{kind} {selection}");
            }
        }
    }

    phase nofollow_grants_ownership_modes_ambiguity_and_padding;
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("media");
        let out = temp.path().join("out");
        private(&root).unwrap();
        private(&out).unwrap();
        media(&root).unwrap();
        let alias = temp.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(!extract(&alias, &out).unwrap().status.success());
        fs::set_permissions(&out, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!extract(&root, &out).unwrap().status.success());
        fs::set_permissions(&out, fs::Permissions::from_mode(0o700)).unwrap();
        let second = temp.path().join("second");
        private(&second).unwrap();
        media(&second).unwrap();
        assert!(
            !run(
                &[
                    "--source-root",
                    root.to_str().unwrap(),
                    "--source-root",
                    second.to_str().unwrap(),
                    "--out-dir",
                    out.to_str().unwrap()
                ],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
        assert!(!out.join("prepared-setup.sealed").exists());
        let file = root.join(".tilo-installer/prepared-setup.sealed");
        let mut padded = sealed();
        padded.extend([b' '; 100]);
        write(&file, &padded).unwrap();
        assert!(extract(&root, &out).unwrap().status.success());
        assert_eq!(
            fs::read(out.join("prepared-setup.sealed")).unwrap(),
            sealed()
        );
        let out2 = temp.path().join("out2");
        private(&out2).unwrap();
        padded.push(0);
        write(&file, &padded).unwrap();
        assert!(!extract(&root, &out2).unwrap().status.success());
        assert!(!out2.join("prepared-setup.sealed").exists());
        let out3 = temp.path().join("out3");
        private(&out3).unwrap();
        fs::remove_file(&file).unwrap();
        symlink("/etc/passwd", &file).unwrap();
        assert!(!extract(&root, &out3).unwrap().status.success());
    }

    phase authenticated_closed_mutations_redact_and_echo_only_success;
    {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().to_str().unwrap();
        let good = b"{\"schema\":1,\"setup_binding\":\"example-private-credential\"}";
        let output = run(&["preflight-authenticated", "--source-dir", source], good).unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, good);
        let values = [
            json!({"schema":true}),
            json!({"schema":1,"completed_applications_only":1}),
            json!({"schema":1,"completed_home":{},"setup_binding":"example-private-credential"}),
            json!({"schema":1,"pending_computer_applications_scope":{}}),
            json!({"schema":1,"planned_btrfs_resize":{}}),
        ];
        for value in values {
            let input = serde_json::to_vec(&value).unwrap();
            let n = run(&["preflight-authenticated", "--source-dir", source], &input).unwrap();
            assert!(!n.status.success());
            assert!(n.stdout.is_empty());
            assert!(!String::from_utf8_lossy(&n.stderr).contains("example-private-credential"));
        }
    }

    phase target_symlink_chain_and_receipt_permissions_refused;
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("media");
        let out = temp.path().join("out");
        let target = temp.path().join("var");
        let escape = temp.path().join("escape");
        for p in [&root, &out, &target, &escape] {
            private(p).unwrap();
        }
        media(&root).unwrap();
        assert!(extract(&root, &out).unwrap().status.success());
        symlink(&escape, target.join("lib")).unwrap();
        assert!(
            !run(
                &[
                    "retain",
                    "--source-dir",
                    out.to_str().unwrap(),
                    "--target-var",
                    target.to_str().unwrap()
                ],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
        assert_eq!(fs::read_dir(escape).unwrap().count(), 0);
        fs::set_permissions(out.join("receipt.json"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            !run(
                &["preflight-profile", "--source-dir", out.to_str().unwrap()],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
    }

    phase browser_padding_and_writable_profiles_are_closed;
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("media");
        let out = temp.path().join("out");
        private(&root).unwrap();
        private(&out).unwrap();
        media(&root).unwrap();
        selected_payload(&root, "browser").unwrap();
        let path = root.join(".tilo-installer/browser-selection.json");
        let original = fs::read(&path).unwrap();
        for prefix in *b" \t\n" {
            let native = temp.path().join(format!("native-{prefix}"));
            private(&native).unwrap();
            write(&path, &[&[prefix], original.as_slice()].concat()).unwrap();
            let n = extract(&root, &native).unwrap();
            assert!(!n.status.success());
        }
        write(&path, &original).unwrap();
        assert!(extract(&root, &out).unwrap().status.success());
        let profile = out.join("profile.json");
        write(&profile, b"{\"schema\":1}").unwrap();
        fs::set_permissions(&profile, fs::Permissions::from_mode(0o622)).unwrap();
        assert!(
            !run(
                &["preflight-profile", "--source-dir", out.to_str().unwrap()],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
    }

    phase authenticated_completed_selection_and_scope_mutations_are_refused;
    {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("media");
        let out = temp.path().join("out");
        private(&root).unwrap();
        private(&out).unwrap();
        media(&root).unwrap();
        let home = selected_payload(&root, "home").unwrap();
        assert!(extract(&root, &out).unwrap().status.success());
        let base = json!({"schema":1,"setup_binding":"example setup","completed_home":home,"completed_setup":receipt(&sealed())});
        let input = serde_json::to_vec(&base).unwrap();
        let output = run(
            &[
                "preflight-authenticated",
                "--source-dir",
                out.to_str().unwrap(),
            ],
            &input,
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, input);
        let mut mutations = Vec::new();
        for key in ["completed_home", "completed_setup", "setup_binding"] {
            for bad in [
                Value::Null,
                json!(true),
                json!({}),
                json!("example-private-credential"),
            ] {
                let mut value = base.clone();
                value[key] = bad;
                mutations.push(value);
            }
        }
        let mut scope = base;
        scope["computer_apps_scope"] = json!({"schema":1});
        mutations.push(scope);
        for value in mutations {
            let input = serde_json::to_vec(&value).unwrap();
            let n = run(
                &[
                    "preflight-authenticated",
                    "--source-dir",
                    out.to_str().unwrap(),
                ],
                &input,
            )
            .unwrap();
            assert!(!n.status.success(), "{value}");
            assert!(n.stdout.is_empty());
        }
    }

    phase cli_rejects_unrelated_options_and_completed_directory_json_authority;
    {
        assert!(
            !run(&["selected-generation", "--source-dir", "/"], &[])
                .unwrap()
                .status
                .success()
        );
        assert!(
            !run(&["preflight-home", "--source-dir", "/", "--encrypted"], &[])
                .unwrap()
                .status
                .success()
        );
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("media");
        let out = temp.path().join("out");
        private(&root).unwrap();
        private(&out).unwrap();
        media(&root).unwrap();
        selected_payload(&root, "home").unwrap();
        assert!(extract(&root, &out).unwrap().status.success());
        let path = out.join("portable-home.json");
        let mut locator: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        locator["binding"] =
            json!({"kind":"completed_generation","attempt":"12345678-1234-4234-8234-123456789abc"});
        write(&path, &serde_json::to_vec(&locator).unwrap()).unwrap();
        assert!(
            !run(
                &["preflight-home", "--source-dir", out.to_str().unwrap()],
                &[]
            )
            .unwrap()
            .status
            .success()
        );
    }
}

fn extract_and_retain(kind: &str, root: &Path, out: &Path, target: &Path) -> TestResult {
    let extracted = extract(root, out)?;
    if !extracted.status.success() {
        return Err(format!(
            "{kind} extract failed: {}",
            String::from_utf8_lossy(&extracted.stderr).trim()
        )
        .into());
    }
    let retained = run(
        &[
            "retain-home",
            "--source-dir",
            out.to_str().ok_or("utf-8 out path")?,
            "--target-var",
            target.to_str().ok_or("utf-8 target path")?,
        ],
        &[],
    )?;
    if !retained.status.success() {
        return Err(format!(
            "{kind} retain-home failed: {}",
            String::from_utf8_lossy(&retained.stderr).trim()
        )
        .into());
    }
    Ok(())
}

fn check_backend_home_selection(
    setup_receipt: &imager_backend::setup::media_setup::DeliveryReceipt,
) -> TestResult {
    use imager_backend::setup::portable_home_media::{HomeSource, MediaGrant};
    use std::sync::atomic::AtomicBool;
    use zeroize::Zeroizing;

    let cancel = AtomicBool::new(false);
    let temp = tempfile::tempdir()?;
    let source_home = temp.path().join("source-home");
    let root = temp.path().join("media");
    let out = temp.path().join("out");
    let target = temp.path().join("var");
    private_directories(&[&source_home, &root, &out, &target])?;
    fs::write(source_home.join("document.txt"), b"real home bytes")?;
    media(&root)?;
    let grant = MediaGrant::open(&root)?;
    let expected = HomeSource::review(&source_home)?.export_to(
        &grant,
        "home_op-1",
        "prepared-binding",
        Zeroizing::new(b"portable home fixture secret".to_vec()),
        &cancel,
    )?;
    grant.publish_expectation(&expected, &cancel)?;
    grant.publish_setup_selection(setup_receipt, &expected, &cancel)?;

    extract_and_retain("home", &root, &out, &target)?;
    let stored: imager_backend::setup::portable_home_media::Expectation = serde_json::from_slice(
        &fs::read(target.join("lib/tilo/prepared-home/expectation.json"))?,
    )?;
    if stored != expected {
        return Err("retained home expectation differs from backend expectation".into());
    }
    Ok(())
}

fn check_backend_applications_selection(
    setup_receipt: &imager_backend::setup::media_setup::DeliveryReceipt,
) -> TestResult {
    use imager_backend::setup::{
        flatpak_transfer::{
            Installation, InstalledRef,
            portable::{self as flatpak_portable, PortableFlatpakSource, StageGrant},
        },
        portable_home_media::MediaGrant,
        portable_transfer::{ImportReview, Limits, Purpose},
    };
    use std::sync::atomic::AtomicBool;
    use zeroize::Zeroizing;

    let cancel = AtomicBool::new(false);
    let temp = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let work = temp.path().canonicalize()?;
    let fake = flatpak_portable::tests::fixture(&work.join("flatpak-source"));
    let stage = work.join("stage");
    let journals = work.join("journals");
    let root = work.join("media");
    let out = work.join("out");
    let target = work.join("var");
    private_directories(&[&stage, &journals, &root, &out, &target])?;
    media(&root)?;
    let source = PortableFlatpakSource::review(
        &fake,
        &fake.source_home,
        vec![InstalledRef {
            reference: "app/org.example.Paint/x86_64/stable".into(),
            origin: "flathub".into(),
            installation: Installation::System("default".into()),
            name: "Paint".into(),
        }],
    )?;
    let staged = source.stage(
        &fake,
        StageGrant {
            root: &stage,
            journals: &journals,
            allocation_bytes: 64 * 1024 * 1024,
            include_user_state: false,
        },
        &cancel,
    )?;
    let operation = "11111111-1111-4111-8111-111111111111".to_owned();
    let password = b"applications transfer fixture secret";
    let limits = Limits {
        plaintext_bytes: 64 * 1024 * 1024,
        entries: 4096,
        metadata_bytes: 8 * 1024 * 1024,
    };
    let staging_package = work.join(format!("applications-{operation}.sealed"));
    let expected = staged.seal(
        &staging_package,
        &operation,
        "prepared-binding",
        Zeroizing::new(password.to_vec()),
        limits,
        &cancel,
    )?;
    let mut archive = ImportReview::review(
        &staging_package,
        password,
        Purpose::FlatpakApplications,
        limits,
        &cancel,
    )?;
    let grant = MediaGrant::open(&root)?;
    grant.deliver_applications(&mut archive, &expected, &cancel)?;
    grant.publish_applications_selection(setup_receipt, &expected, &cancel)?;

    extract_and_retain("applications", &root, &out, &target)?;
    let stored: flatpak_portable::Expectation = serde_json::from_slice(&fs::read(
        target.join("lib/tilo/prepared-applications/expectation.json"),
    )?)?;
    if stored != expected {
        return Err("retained applications expectation differs from backend expectation".into());
    }
    Ok(())
}

fn check_backend_browser_selection(
    setup_receipt: &imager_backend::setup::media_setup::DeliveryReceipt,
) -> TestResult {
    use imager_backend::{
        browser_selection::SelectedSources,
        setup::{
            account_transfer::{self, SourceRoots},
            browser_profiles,
            browser_transfer::{self, ExportRequest},
            portable_home_media::MediaGrant,
        },
    };
    use std::sync::{Arc, atomic::AtomicBool};
    use zeroize::Zeroizing;

    let cancel = AtomicBool::new(false);
    let temp = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let source_home = temp.path().join("source-home");
    let profile = source_home.join(".config/chromium/Default");
    let root = temp.path().join("media");
    let out = temp.path().join("out");
    let target = temp.path().join("var");
    private_directories(&[&source_home, &profile, &root, &out, &target])?;
    fs::write(profile.join("Bookmarks"), b"selected bookmarks")?;
    media(&root)?;
    let inventory = account_transfer::discover(&SourceRoots {
        home: source_home.clone(),
        config: source_home.join(".config"),
    });
    let selected = SelectedSources::from_reviewed(
        browser_profiles::discover(&source_home, &inventory.candidates)?
            .into_iter()
            .map(|source| {
                let profile = source.profile.clone();
                (Arc::new(source), profile)
            })
            .collect(),
    )?;
    let plan = selected.plan("prepared-binding".into(), [3; 32])?;
    let operation = "22222222-2222-4222-8222-222222222222".to_owned();
    let staging_package = temp.path().join(format!("browser-{operation}.sealed"));
    let expected = browser_transfer::export(
        &selected,
        &plan,
        ExportRequest {
            staging_parent: temp.path(),
            package: &staging_package,
            operation: &operation,
            password: Zeroizing::new(b"browser transfer fixture secret".to_vec()),
        },
        &cancel,
    )?;
    let mut archive = browser_transfer::review(
        &expected,
        &plan,
        &staging_package,
        b"browser transfer fixture secret",
        &cancel,
    )?;
    let grant = MediaGrant::open(&root)?;
    grant.deliver_browser(&mut archive, &expected, &cancel)?;
    grant.publish_browser_selection(setup_receipt, &expected, &cancel)?;

    extract_and_retain("browser", &root, &out, &target)?;
    let stored: browser_transfer::Expectation = serde_json::from_slice(&fs::read(
        target.join("lib/tilo/prepared-browser/expectation.json"),
    )?)?;
    if stored != expected {
        return Err("retained browser expectation differs from backend expectation".into());
    }
    Ok(())
}

#[test]
fn backend_produced_medium_selections_extract_and_retain() {
    use imager_backend::setup::media_setup::DeliveryReceipt;
    use std::path::PathBuf;

    let setup_data = sealed();
    let setup_receipt = DeliveryReceipt {
        relative_path: PathBuf::from(".tilo-installer/prepared-setup.sealed"),
        sealed_bytes: setup_data.len() as u64,
        ciphertext_sha256: hex::encode(Sha256::digest(&setup_data)),
    };
    let failures: Vec<String> = [
        check_backend_home_selection(&setup_receipt),
        check_backend_applications_selection(&setup_receipt),
        check_backend_browser_selection(&setup_receipt),
    ]
    .into_iter()
    .filter_map(|result| result.err().map(|error| error.to_string()))
    .collect();
    assert!(
        failures.is_empty(),
        "backend-produced medium selections rejected by extractor: {failures:?}"
    );
}

fn check_flat_payload_round_trip(kind: &str) -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("media");
    let out = temp.path().join("out");
    let target = temp.path().join("var");
    private_directories(&[&root, &out, &target])?;
    media(&root)?;
    let data = sealed();
    let flat = flat_expectation(kind, &data)?;
    let mut canonical = expectation(kind, &data)?;
    if kind == "applications" {
        canonical
            .get_mut("kind")
            .and_then(Value::as_object_mut)
            .ok_or("missing applications kind object")?
            .insert("consumer_protocol".into(), json!(1));
    }
    selected_payload_with(&root, kind, &data, &flat)?;

    let extracted = extract(&root, &out)?;
    if !extracted.status.success() {
        return Err(format!("flat {kind} extract failed").into());
    }
    let locator_path = out.join(format!("portable-{kind}.json"));
    let mut locator: Value = serde_json::from_slice(&fs::read(&locator_path)?)?;
    if locator.get("expectation") != Some(&canonical) {
        return Err(format!("staged {kind} locator did not normalize to kind").into());
    }

    // Overwrite staged locator with flat expectation to exercise flat locator readers too.
    locator
        .as_object_mut()
        .ok_or("locator must be an object")?
        .insert("expectation".into(), flat.clone());
    write(&locator_path, &serde_json::to_vec(&locator)?)?;
    let out_str = out.to_str().ok_or("utf-8 out path")?;
    let target_str = target.to_str().ok_or("utf-8 target path")?;
    if !run(&["preflight-home", "--source-dir", out_str], &[])?
        .status
        .success()
    {
        return Err(format!("flat {kind} locator preflight-home failed").into());
    }
    if kind == "home" || kind == "applications" {
        let auth = if kind == "home" {
            json!({"schema":1,"setup_binding":"example setup","completed_home":flat,"completed_setup":receipt(&data)})
        } else {
            json!({"schema":1,"setup_binding":"example setup","completed_applications_only":true,"completed_applications":flat,"completed_setup":receipt(&data)})
        };
        let auth_bytes = serde_json::to_vec(&auth)?;
        let checked = run(
            &["preflight-authenticated", "--source-dir", out_str],
            &auth_bytes,
        )?;
        if !checked.status.success() || checked.stdout != auth_bytes {
            return Err(format!("flat {kind} preflight-authenticated failed").into());
        }
    }

    if !run(
        &[
            "retain-home",
            "--source-dir",
            out_str,
            "--target-var",
            target_str,
        ],
        &[],
    )?
    .status
    .success()
    {
        return Err(format!("flat {kind} retain-home failed").into());
    }
    let retained_path = target.join(format!("lib/tilo/prepared-{kind}/expectation.json"));
    if serde_json::from_slice::<Value>(&fs::read(&retained_path)?)? != canonical {
        return Err(format!("retained {kind} expectation did not write canonical kind").into());
    }

    // Retry retain-home against an existing flat retained expectation.json.
    write(&retained_path, &serde_json::to_vec(&flat)?)?;
    if !run(
        &[
            "retain-home",
            "--source-dir",
            out_str,
            "--target-var",
            target_str,
        ],
        &[],
    )?
    .status
    .success()
    {
        return Err(
            format!("flat {kind} retain-home retry against flat expectation failed").into(),
        );
    }
    Ok(())
}

#[test]
fn flat_medium_selections_extract_authenticate_and_retain_as_kind() {
    let failures: Vec<String> = ["home", "applications", "browser"]
        .into_iter()
        .filter_map(|kind| {
            check_flat_payload_round_trip(kind)
                .err()
                .map(|error| error.to_string())
        })
        .collect();
    assert!(
        failures.is_empty(),
        "flat medium selections rejected by extractor: {failures:?}"
    );
}
