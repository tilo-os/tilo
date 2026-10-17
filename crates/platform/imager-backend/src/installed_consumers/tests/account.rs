//! Account and optional service consumers against a compiled ELF fixture
//! runtime. Signature verification is the runtime-bundle tool's own tested
//! boundary and is stubbed here; the linked probe really executes through the
//! bundle's own loader. Not native Tilo qualification.
use super::super::{
    account::{self, Produce},
    json, service,
};
use crate::account_admission::RECEIVING_DEPENDENCIES;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

const VERSION: &str = "1+native.fixture";

struct Runtime {
    _temp: tempfile::TempDir,
    root: PathBuf,
    probe: PathBuf,
}

fn executable(path: &Path, contents: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn compile(arguments: &[&std::ffi::OsStr]) {
    let output = Command::new("cc").args(arguments).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Runtime {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let share = root.join("usr/share/tilo");
        std::fs::create_dir_all(&share).unwrap();
        std::fs::write(share.join("runtime-version"), format!("{VERSION}\n")).unwrap();
        std::fs::write(
            share.join("runtime-bundle-publisher.hex"),
            "ab".repeat(32) + "\n",
        )
        .unwrap();
        let bundle = root.join("usr/lib/tilo-runtime").join(VERSION);
        let lib = bundle.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::create_dir_all(bundle.join("bin")).unwrap();
        let library = lib.join("libruntime_dylib.so");
        let sdk = root.join("sdk.c");
        std::fs::write(&sdk, "int protocol(void) { return 1; }\n").unwrap();
        compile(&[
            "-shared".as_ref(),
            "-fPIC".as_ref(),
            "-Wl,-soname,libruntime_dylib.so".as_ref(),
            sdk.as_os_str(),
            "-o".as_ref(),
            library.as_os_str(),
        ]);
        let probe = bundle.join("bin/tilo-runtime-account-capability");
        let source = root.join("probe.c");
        std::fs::write(
            &source,
            "#include <stdio.h>\nextern int protocol(void);\nint main(void) { \
             printf(\"{\\\"schema\\\":1,\\\"protected_grant_protocol\\\":%d}\\n\", protocol()); }\n",
        )
        .unwrap();
        let search = OsString::from(format!("-L{}", lib.display()));
        compile(&[
            source.as_os_str(),
            "-o".as_ref(),
            probe.as_os_str(),
            search.as_os_str(),
            "-lruntime_dylib".as_ref(),
        ]);
        let loader_name = account::interpreter(&probe).unwrap();
        let loader = bundle.join(&loader_name);
        std::fs::create_dir_all(loader.parent().unwrap()).unwrap();
        std::fs::copy(
            Path::new("/").join(loader_name.strip_prefix("lib/").unwrap()),
            &loader,
        )
        .unwrap();
        let report = Command::new("ldd")
            .arg(&probe)
            .env("LD_LIBRARY_PATH", &lib)
            .output()
            .unwrap();
        for word in String::from_utf8(report.stdout).unwrap().split_whitespace() {
            let candidate = Path::new(word.trim_matches(|c| c == '(' || c == ')'));
            if candidate.is_absolute()
                && candidate
                    .file_name()
                    .is_some_and(|name| name != "libruntime_dylib.so")
                && candidate.is_file()
            {
                std::fs::copy(candidate, lib.join(candidate.file_name().unwrap())).unwrap();
            }
        }
        executable(
            &root.join("usr/libexec/tilo-account-credential"),
            b"#!/bin/sh\n[ \"$1\" = capabilities ] || exit 64\n\
              printf '{\"schema\":1,\"protected_grant_protocol\":1}\\n'\n",
        );
        let inventory = [
            "bin/tilo-runtime-account-capability",
            "lib/libruntime_dylib.so",
            &loader_name,
        ]
        .map(|name| format!("[[files]]\npath=\"{name}\"\n"))
        .concat();
        std::fs::write(
            bundle.join("bundle.toml"),
            format!(
                "version=\"{VERSION}\"\nroot_hash=\"{}\"\n{inventory}",
                "cd".repeat(32)
            ),
        )
        .unwrap();
        Self {
            _temp: temp,
            root,
            probe,
        }
    }
}

fn stub_signature(
    calls: &mut Vec<Vec<OsString>>,
    command: &[OsString],
    env: &[(&str, OsString)],
) -> io::Result<Vec<u8>> {
    calls.push(command.to_vec());
    if command[0]
        .to_string_lossy()
        .ends_with("/tilo-runtime-bundle")
    {
        let required = ["--require-signature", "--require-publisher"]
            .iter()
            .all(|flag| command.iter().any(|arg| arg == flag));
        return if required {
            Ok(Vec::new())
        } else {
            Err(io::Error::other(
                "runtime verification without signature policy",
            ))
        };
    }
    account::invoke(command, env)
}

fn forbidden_qualifier(_: &[OsString], _: &[(&str, OsString)]) -> io::Result<Vec<u8>> {
    Err(io::Error::other(
        "optional service qualification executed without request",
    ))
}

fn forbidden_route(_: &[OsString], _: &[(&str, OsString)], _: &[u8]) -> io::Result<Vec<u8>> {
    Err(io::Error::other("receiving route executed without request"))
}

fn produce(root: &Path, runner: &mut account::Runner<'_>) -> io::Result<Value> {
    let mut qualifier = forbidden_qualifier;
    let mut route = forbidden_route;
    account::produce(Produce {
        root,
        runner,
        qualify_services: true,
        service_evidence: None,
        qualifier: &mut qualifier,
        route_qualifier: &mut route,
    })
}

#[test]
fn root_producer_drops_privilege_without_disabling_cef_sandbox() {
    let command = [
        "/usr/bin/python3",
        service::QUALIFICATION_SCRIPT,
        "/signed/host",
    ]
    .map(OsString::from);
    let dropped = service::dropped_for_root(&command);
    assert_eq!(
        dropped[..5],
        [
            "/usr/bin/setpriv",
            "--reuid=65534",
            "--regid=65534",
            "--clear-groups",
            "--"
        ]
        .map(OsString::from)
    );
    assert_eq!(dropped[5..], command);
    assert!(!dropped.iter().any(|arg| arg == "--no-sandbox"));
}

#[test]
fn cloud_consumer_checks_execute_without_optional_service_tools() {
    let runtime = Runtime::new();
    let mut calls = Vec::new();
    let receipt = produce(&runtime.root, &mut |command, env| {
        stub_signature(&mut calls, command, env)
    })
    .unwrap();
    assert!(receipt.get("service_sessions").is_none());
    assert_eq!(receipt["protected_grant_protocol"], 1);
    assert_eq!(
        receipt["runtime_probe_sha256"],
        hex::encode(Sha256::digest(std::fs::read(&runtime.probe).unwrap())).as_str()
    );
    assert_eq!(calls.len(), 4);
    assert!(calls[1].iter().any(|arg| arg == "--inhibit-cache"));
    assert_eq!(calls[2][1..], [OsString::from("capabilities")]);

    // A probe linked against another SDK protocol is refused.
    let mut changed = runtime.root.clone();
    changed.push("usr/libexec/tilo-account-credential");
    executable(
        &changed,
        b"#!/bin/sh\nprintf '{\"schema\":1,\"protected_grant_protocol\":2}\\n'\n",
    );
    assert!(
        produce(&runtime.root, &mut |command, env| {
            stub_signature(&mut Vec::new(), command, env)
        })
        .is_err()
    );
}

#[test]
fn partial_optional_qualification_refuses_cloud_receipt() {
    let runtime = Runtime::new();
    for name in [service::RECEIVING_ENTRY, service::QUALIFICATION_SCRIPT] {
        let path = runtime.root.join(name.strip_prefix('/').unwrap());
        executable(&path, b"not qualification");
        let error = produce(&runtime.root, &mut |command, env| {
            stub_signature(&mut Vec::new(), command, env)
        })
        .unwrap_err();
        assert!(error.to_string().contains("incomplete optional"), "{error}");
        std::fs::remove_file(path).unwrap();
    }
}

fn service_produce(
    root: &Path,
    evidence: Option<&Value>,
    qualify: bool,
) -> io::Result<Option<Value>> {
    let sdk = service::Sdk {
        runtime_version: VERSION.into(),
        runtime_root_hash: "cd".repeat(32),
        runtime_probe_sha256: "1".repeat(64),
        runtime_library_sha256: "2".repeat(64),
        runtime_library: "lib/libruntime_dylib.so".into(),
    };
    let mut runner = forbidden_qualifier;
    let mut qualifier = forbidden_qualifier;
    let mut route = forbidden_route;
    service::produce(service::Produce {
        root,
        runner: &mut runner,
        sdk: &sdk,
        qualify,
        evidence,
        qualifier: &mut qualifier,
        route_qualifier: &mut route,
    })
}

#[test]
fn saved_service_claim_cannot_downgrade_to_cloud_only() {
    let runtime = Runtime::new();
    let error = service_produce(
        &runtime.root,
        Some(&json!({"schema": 1, "protocol": 1})),
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("qualified sandbox"), "{error}");
    let mut saved = json!({"schema": service::SERVICE_SCHEMA});
    for key in service::ROUTE_KEYS {
        saved[key] = "ab".repeat(32).into();
    }
    saved["receiving_shell_path"] = "/usr/bin/shell".into();
    saved["receiving_dependencies"] = json!({});
    saved["receiving_python_path"] = "/usr/bin/python3".into();
    let error = service_produce(&runtime.root, Some(&saved), false).unwrap_err();
    assert!(error.to_string().contains("no longer installed"), "{error}");
}

#[test]
fn readonly_cloud_check_skips_optional_receivers() {
    let runtime = Runtime::new();
    assert!(
        service_produce(&runtime.root, None, false)
            .unwrap()
            .is_none()
    );
}

/// A real measured receiving closure over fixture files, as the saved account
/// receipt reader returns it. Only the owned sandbox route callback is replaced
/// by its explicit test response; this is serialization evidence, not native
/// readiness.
pub(crate) fn route_contract_receipt() -> Vec<u8> {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    for path in RECEIVING_DEPENDENCIES
        .iter()
        .copied()
        .chain(["/usr/bin/shell", "/usr/bin/python3.13"])
    {
        executable(
            &root.join(path.strip_prefix('/').unwrap()),
            format!("fixture: {path}").as_bytes(),
        );
    }
    std::os::unix::fs::symlink("python3.13", root.join("usr/bin/python3")).unwrap();
    let sdk = service::Sdk {
        runtime_version: "2026.10".into(),
        runtime_root_hash: "2".repeat(64),
        runtime_probe_sha256: "3".repeat(64),
        runtime_library_sha256: "4".repeat(64),
        runtime_library: "lib/libruntime_dylib.so".into(),
    };
    let mut receipt = json!({
        "schema": 1, "protected_grant_protocol": 1,
        "credential_helper_sha256": "1".repeat(64), "runtime_root_hash": sdk.runtime_root_hash,
        "runtime_probe_sha256": sdk.runtime_probe_sha256,
        "runtime_library_sha256": sdk.runtime_library_sha256,
        "runtime_version": sdk.runtime_version, "runtime_library": sdk.runtime_library,
    });
    let candidate = json!({
        "schema": 1, "protocol": 2, "sdk_runtime_root_hash": sdk.runtime_root_hash,
        "browser_package_hash": "5".repeat(64), "browser_publisher": "6".repeat(64),
        "browser_binary_sha256": "7".repeat(64), "cef_runtime_version": "2026.10-chromium",
        "cef_runtime_root_hash": "8".repeat(64), "cef_runtime_publisher": "9".repeat(64),
        "cef_host_sha256": "a".repeat(64), "qualification_script_sha256": "b".repeat(64),
        "origin_storage_qualification_sha256": "b".repeat(64),
    });
    // The default callback must never fabricate a successful receiving route.
    let mut native = service::native_route_qualify;
    assert!(
        service::qualify_receiving_route(&root, &sdk, &candidate, "/usr/bin/shell", &mut native)
            .is_err()
    );
    let mut fixture = |command: &[OsString], env: &[(&str, OsString)], request: &[u8]| {
        assert_eq!(command[0], root.join("usr/bin/python3.13").into_os_string());
        assert_eq!(
            command[1..],
            [
                "-I",
                service::RECEIVING_ENTRY,
                "--service-session-qualification"
            ]
            .map(|arg| if arg == service::RECEIVING_ENTRY {
                root.join(arg.strip_prefix('/').unwrap()).into_os_string()
            } else {
                arg.into()
            })
        );
        assert_eq!(env.len(), 3);
        let facts = json::strict(request).unwrap();
        Ok(json::canonical(&json!({
            "schema": 2, "service_session_protocol": 2,
            "receiving_route_qualification_sha256": facts["receiving_route_qualification_sha256"],
            "private_list_verified": true, "private_resolve_verified": true,
            "isolated_profile_verified": true, "revocation_verified": true,
            "no_vault_mount_verified": true, "origin_storage_persisted": true,
            "chat_destination_navigation_verified": true,
        })))
    };
    receipt["service_sessions"] =
        service::qualify_receiving_route(&root, &sdk, &candidate, "/usr/bin/shell", &mut fixture)
            .unwrap();
    let path = root.join("receipt.json");
    std::fs::write(&path, json::canonical_ascii(&receipt)).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    // Exercise the installed check's strict saved-receipt reader too.
    let read = account::read_receipt(&path, &receipt).unwrap();
    let bytes = json::canonical_ascii(&read);
    assert!(bytes.len() < 16 * 1024);
    bytes
}
