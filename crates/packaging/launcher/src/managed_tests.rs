use crate::managed::*;
use std::os::unix::fs::PermissionsExt;

fn installed() -> (tempfile::TempDir, InstalledManagedPackage, PackageTrust) {
    installed_bytes(b"#!/bin/sh\nexit 0\n")
}
fn installed_bytes(binary: &[u8]) -> (tempfile::TempDir, InstalledManagedPackage, PackageTrust) {
    let root = tempfile::tempdir().unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
    let public = key.verifying_key().to_bytes();
    let hex: String = public.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    });
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.agent"
version = "1.0.0"
kind = "service"
runtime = "tilo-runtime/2026.07"
[publisher]
identity = "ed25519:{hex}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[sandbox.services]
collab = "required@1"
model-gateway = "required@1"
[entrypoint]
bin = "bin/agent"
type = "cli"
"#
    ))
    .unwrap();
    let payload = root.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/agent"), binary).unwrap();
    std::fs::set_permissions(
        payload.join("bin/agent"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let packed = tilo_app_package::pack(
        &payload,
        &manifest,
        &tilo_app_package::SigningIdentity::from_signing_key(key),
    )
    .unwrap();
    let id = packed.id();
    let signed = packed.meta.signed_root_bytes();
    let manifest_bytes = packed.meta.manifest_bytes.clone();
    std::fs::write(root.path().join("manifest.toml"), &manifest_bytes).unwrap();
    std::fs::write(payload.join("manifest.toml"), &manifest_bytes).unwrap();
    let evidence = root.path().join("integrity/so.tilo.agent");
    std::fs::create_dir_all(&evidence).unwrap();
    std::fs::write(evidence.join("signed-root"), signed).unwrap();
    std::fs::write(evidence.join("manifest.toml"), manifest_bytes).unwrap();
    std::fs::write(
        evidence.join("collection.qcl"),
        &packed.meta.collection_bytes,
    )
    .unwrap();
    let trust = PackageTrust {
        publishers: vec![public],
        attestors: vec![],
        revoked: tilo_app_package::trust::RevocationList::default(),
    };
    let package = InstalledManagedPackage::verify(
        agent_package_row(id.to_hex(), payload, root.path().join("manifest.toml")),
        root.path().to_path_buf(),
        id,
        &trust,
    )
    .unwrap();
    (root, package, trust)
}

#[test]
fn managed_package_binds_installed_bytes_and_independent_trust() {
    let (_root, package, trust) = installed();
    package.revalidate(&trust).unwrap();
    let untrusted = PackageTrust {
        publishers: vec![],
        attestors: vec![],
        revoked: tilo_app_package::trust::RevocationList::default(),
    };
    assert!(
        package.revalidate(&untrusted).is_err(),
        "embedded key is not trust"
    );
    std::fs::write(
        package.payload_root().join("bin/agent"),
        b"modified executable",
    )
    .unwrap();
    assert!(
        package.revalidate(&trust).is_err(),
        "signed archive cannot bless modified installed executable"
    );
}

#[test]
fn managed_package_rejects_replaced_manifest_and_escaping_executable() {
    let (_root, package, trust) = installed();
    let manifest = std::fs::read(package.manifest_path()).unwrap();
    std::fs::write(package.manifest_path(), b"replaced").unwrap();
    assert!(package.revalidate(&trust).is_err());
    std::fs::write(package.manifest_path(), manifest).unwrap();
    let binary = package.payload_root().join("bin/agent");
    let bytes = std::fs::read(&binary).unwrap();
    let outside = package.payload_root().parent().unwrap().join("outside");
    std::fs::write(&outside, bytes).unwrap();
    std::fs::remove_file(&binary).unwrap();
    std::os::unix::fs::symlink(outside, binary).unwrap();
    assert!(
        package.revalidate(&trust).is_err(),
        "same bytes through escaping link are not the installed executable"
    );
}

#[test]
fn managed_descriptors_are_bound_and_consumed_once() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let root = tempfile::tempdir().unwrap();
    let binding = ManagedBinding {
        room: uuid::Uuid::new_v4().to_string(),
        app: WORKSPACE_AGENT_PACKAGE_ID.into(),
        agent: "agent-1".into(),
        task: "task-1".into(),
        run: "run-1".into(),
        launch: "launch-1".into(),
        generation: 1,
    };
    let (identity, _identity_peer) = UnixStream::pair().unwrap();
    let (collab, mut peer) = UnixStream::pair().unwrap();
    let (model, _model_peer) = UnixStream::pair().unwrap();
    let mut context = crate::services::EnrolledServiceContext::new(
        binding.clone(),
        root.path().to_path_buf(),
        "epoch-1".into(),
        "channel-1".into(),
        crate::services::ManagedProviderSelection {
            profile_id: "profile-1".into(),
            model_id: "model-1".into(),
            protocol: "anthropic".into(),
        },
        identity,
        collab,
        model,
    )
    .unwrap();
    let required = [
        ("collab".into(), "required@1".into()),
        ("model-gateway".into(), "required@1".into()),
    ]
    .into_iter()
    .collect();
    let mut wrong = binding.clone();
    wrong.agent = "other".into();
    assert!(context.take_for_launch(&wrong, &required).is_err());
    let services = context.take_for_launch(&binding, &required).unwrap();
    assert!(context.take_for_launch(&binding, &required).is_err());
    peer.write_all(b"proof").unwrap();
    let service = services.into_iter().find(|s| s.name == "collab").unwrap();
    let mut stream = UnixStream::from(service.host);
    let mut read = [0; 5];
    stream.read_exact(&mut read).unwrap();
    assert_eq!(&read, b"proof");
}

fn runtime(root: &std::path::Path) -> ManagedRuntime {
    let source = root.join("runtime-source");
    let relative = runtime_bundle::host_minimal_paths_for(std::env::consts::ARCH).unwrap()[0]
        .trim_start_matches('/');
    let path = source.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"unit-test runtime bytes; never executed").unwrap();
    let identity = root.join("runtime-identity.json");
    std::fs::write(
        &identity,
        format!("{{\"secret_key_hex\":\"{}\"}}", "20".repeat(32)),
    )
    .unwrap();
    let runtime_root = root.join("runtime");
    let built = runtime_bundle::build_bundle_with(
        &runtime_root,
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: Some(&source),
            arch: Some(std::env::consts::ARCH),
            identity: Some(&identity),
            extra_libs: &[],
        },
    )
    .unwrap();
    ManagedRuntime {
        bundle_root: runtime_root,
        runtime_publisher: built.publisher_hex.unwrap(),
        sandbox_shim: std::env::current_exe().unwrap(),
        bwrap: std::env::current_exe().unwrap(),
    }
}
struct CurrentAuthority(std::sync::atomic::AtomicBool);
impl ManagedLaunchAuthority for CurrentAuthority {
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        self.revalidate_launch(binding)
    }
    fn revalidate_launch(&self, _: &ManagedBinding) -> Result<(), String> {
        if self.0.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err("revoked owner permission".into())
        }
    }
}
fn binding() -> ManagedBinding {
    ManagedBinding {
        room: uuid::Uuid::new_v4().to_string(),
        app: WORKSPACE_AGENT_PACKAGE_ID.into(),
        agent: "agent-1".into(),
        task: "task-1".into(),
        run: "run-1".into(),
        launch: "launch-1".into(),
        generation: 1,
    }
}
#[test]
fn managed_runtime_lease_cannot_survive_shutdown_or_mint_a_second_attempt() {
    use std::sync::{Arc, atomic::Ordering};
    let (root, package, trust) = installed();
    let authority = Arc::new(CurrentAuthority(true.into()));
    let mut launcher =
        ManagedLauncher::new(package, trust, runtime(root.path()), authority.clone()).unwrap();
    let lease = launcher.runtime_lease();
    let facts = lease.revalidate().unwrap();
    assert_eq!(facts.package_id, "so.tilo.agent");
    assert!(!facts.runtime_epoch.is_empty());
    let original = binding();
    let permit = launcher.authorize(original.clone()).unwrap();
    drop(permit);
    assert!(
        launcher.authorize(original.clone()).is_err(),
        "dropping a permit never grants replay"
    );
    let mut changed = original.clone();
    changed.generation += 1;
    assert!(
        launcher.authorize(changed).is_err(),
        "generation must not bypass launch identity retention"
    );
    authority.0.store(false, Ordering::Release);
    let mut fresh = original;
    fresh.launch = "launch-2".into();
    assert!(launcher.authorize(fresh).is_err());
    launcher.shutdown().unwrap();
    assert!(lease.revalidate().is_err());
}
#[test]
fn managed_runtime_lease_rechecks_signed_runtime_and_package_bytes() {
    let (root, package, trust) = installed();
    let bundle = runtime(root.path());
    let runtime_path = bundle.bundle_root.clone();
    let launcher = ManagedLauncher::new(
        package.clone(),
        trust,
        bundle,
        std::sync::Arc::new(CurrentAuthority(true.into())),
    )
    .unwrap();
    let lease = launcher.runtime_lease();
    lease.revalidate().unwrap();
    let executable = package.payload_root().join("bin/agent");
    let original = std::fs::read(&executable).unwrap();
    std::fs::write(&executable, b"changed").unwrap();
    assert!(lease.revalidate().is_err());
    std::fs::write(&executable, original).unwrap();
    lease.revalidate().unwrap();
    let bundle_manifest = runtime_path.join("bundle.toml");
    std::fs::write(bundle_manifest, b"changed runtime metadata").unwrap();
    assert!(lease.revalidate().is_err());
}

#[test]
fn managed_launch_claim_is_not_repeated_after_success_or_refusal() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct ConsumingAuthority {
        claims: AtomicUsize,
        refuse: bool,
    }
    impl ManagedLaunchAuthority for ConsumingAuthority {
        fn revalidate_launch(&self, _: &ManagedBinding) -> Result<(), String> {
            Ok(())
        }
        fn claim_launch(&self, _: &ManagedBinding) -> Result<(), String> {
            let prior = self.claims.fetch_add(1, Ordering::SeqCst);
            if self.refuse || prior != 0 {
                Err("one-use claim unavailable".into())
            } else {
                Ok(())
            }
        }
    }
    for refuse in [false, true] {
        let (root, package, trust) = installed();
        let authority = Arc::new(ConsumingAuthority {
            claims: AtomicUsize::new(0),
            refuse,
        });
        let mut launcher =
            ManagedLauncher::new(package, trust, runtime(root.path()), authority.clone()).unwrap();
        let original = binding();
        assert_eq!(launcher.authorize(original.clone()).is_err(), refuse);
        assert!(launcher.authorize(original.clone()).is_err());
        let mut changed = original;
        changed.generation += 1;
        assert!(launcher.authorize(changed).is_err());
        assert_eq!(
            authority.claims.load(Ordering::SeqCst),
            1,
            "replays must not consume or request another host claim, even after refusal"
        );
    }
}

#[test]
fn managed_claim_is_consumed_once_and_policy_revocation_stops_reserved_effect() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct Authority {
        claims: AtomicUsize,
        checks: AtomicUsize,
        allowed: AtomicBool,
    }
    impl ManagedLaunchAuthority for Authority {
        fn claim_launch(&self, _: &ManagedBinding) -> Result<(), String> {
            if self.claims.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err("claim consumed".into())
            }
        }
        fn revalidate_launch(&self, _: &ManagedBinding) -> Result<(), String> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            if self.allowed.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err("policy revoked".into())
            }
        }
    }
    let (root, package, trust) = installed();
    let authority = Arc::new(Authority {
        claims: AtomicUsize::new(0),
        checks: AtomicUsize::new(0),
        allowed: AtomicBool::new(true),
    });
    let mut launcher =
        ManagedLauncher::new(package, trust, runtime(root.path()), authority.clone()).unwrap();
    let binding = binding();
    let permit = launcher.authorize(binding.clone()).unwrap();
    permit.revalidate().unwrap();
    permit.revalidate().unwrap();
    assert_eq!(authority.claims.load(Ordering::SeqCst), 1);
    assert!(authority.checks.load(Ordering::SeqCst) >= 2);
    authority.allowed.store(false, Ordering::Release);
    assert!(
        permit.revalidate().is_err(),
        "the pre-spawn gate must reject revoked policy without consuming a new claim"
    );
    drop(permit);
    authority.allowed.store(true, Ordering::Release);
    assert!(
        launcher.authorize(binding).is_err(),
        "revocation never resets the original identity"
    );
    assert_eq!(authority.claims.load(Ordering::SeqCst), 1);
}
