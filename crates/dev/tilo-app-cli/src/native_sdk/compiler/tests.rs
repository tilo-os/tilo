use super::*;

fn signed_fixture() -> Result<(tempfile::TempDir, SdkSelection)> {
    let temp = tempfile::tempdir()?;
    let identity = runtime_bundle::Identity::generate();
    let key = temp.path().join("publisher.json");
    std::fs::write(&key, identity.to_json_bytes())?;
    let extra = temp.path().join("fixture.so");
    std::fs::write(&extra, b"signed compiler fixture payload")?;
    let (manifest, bundle) = runtime_bundle::build_abi_bundle(
        &temp.path().join("bundles"),
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: None,
            arch: Some(std::env::consts::ARCH),
            identity: Some(&key),
            extra_libs: &[extra],
        },
    )?;
    let selected = SdkSelection {
        compiler: None,
        bundle,
        publisher: identity.public_key_hex(),
        root: manifest.root_hash,
    };
    Ok((temp, selected))
}

#[test]
fn signed_compiler_cache_rejects_changed_source_cache_root_and_publisher() -> Result<()> {
    let (temp, selected) = signed_fixture()?;
    let root = temp.path().join("checkout");
    std::fs::create_dir(&root)?;
    let cached = cache(&selected, &root, &root, std::env::consts::ARCH)?;
    anyhow::ensure!(cached != selected.bundle);
    verify_selected(&cached, &selected, std::env::consts::ARCH)?;
    let mut wrong = selected.clone();
    wrong.publisher = runtime_bundle::Identity::generate().public_key_hex();
    anyhow::ensure!(cache(&wrong, &root, &root, std::env::consts::ARCH).is_err());
    wrong = selected.clone();
    wrong.root = "0".repeat(64);
    anyhow::ensure!(cache(&wrong, &root, &root, std::env::consts::ARCH).is_err());
    std::fs::write(cached.join("lib/fixture.so"), b"mutated cached payload")?;
    anyhow::ensure!(cache(&selected, &root, &root, std::env::consts::ARCH).is_err());
    std::fs::copy(
        selected.bundle.join("lib/fixture.so"),
        cached.join("lib/fixture.so"),
    )?;
    std::fs::write(
        selected.bundle.join("lib/fixture.so"),
        b"mutated selected payload",
    )?;
    anyhow::ensure!(cache(&selected, &root, &root, std::env::consts::ARCH).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn signed_compiler_cache_refuses_a_directory_alias() -> Result<()> {
    let (temp, selected) = signed_fixture()?;
    let root = temp.path().join("checkout");
    std::fs::create_dir(&root)?;
    std::os::unix::fs::symlink(temp.path().join("bundles"), root.join(".tilo-native-sdk"))?;
    anyhow::ensure!(cache(&selected, &root, &root, std::env::consts::ARCH).is_err());
    Ok(())
}

#[test]
fn actual_cargo_artifact_admission_rejects_sdk_dependency_rebuilds() -> Result<()> {
    let manifest = Path::new("/owned/authored/Cargo.toml");
    validate_artifact_message(
        &serde_json::json!({"reason":"compiler-artifact","manifest_path":manifest}).to_string(),
        manifest,
    )?;
    let foreign = serde_json::json!({"reason":"compiler-artifact","manifest_path":"/sdk/tilo-app/Cargo.toml"});
    anyhow::ensure!(validate_artifact_message(&foreign.to_string(), manifest).is_err());
    anyhow::ensure!(
        validate_artifact_message(r#"{"reason":"compiler-artifact"}"#, manifest).is_err()
    );
    Ok(())
}

#[test]
fn consumer_manifest_refuses_build_scripts_and_dependency_substitution() -> Result<()> {
    let interface: Interface = serde_json::from_value(serde_json::json!({
        "schema":1,"runtime":{"version":"v","root":"r","publisher":"p","toolkit_sha256":"t"},
        "compiler":"c","host":"h","architecture":"a","profile":"thin-app","ui":"u","resources":[],
        "units":[{"target":"tilo_app","kind":["rlib"],"features":["actual"],"artifacts":[]}]
    }))?;
    validate_dependencies(
        &toml::from_str("[dependencies]\nui = { package = 'tilo-app', features = ['actual'] }")?,
        &interface,
    )?;
    for source in [
        "[dependencies]\nforeign = '1'",
        "[dependencies]\nui = { package = 'tilo-app', features = ['missing'] }",
        "[package]\nbuild = '/foreign/script.rs'",
        "[build-dependencies]\nforeign = '1'",
    ] {
        anyhow::ensure!(
            validate_dependencies(&toml::from_str(source)?, &interface).is_err(),
            "{source}"
        );
    }
    Ok(())
}

#[test]
fn signed_interface_binding_refuses_other_runtime_architecture_and_compiler() -> Result<()> {
    let (_temp, selected) = signed_fixture()?;
    let manifest: runtime_bundle::BundleManifest = toml::from_str(&std::fs::read_to_string(
        selected.bundle.join("bundle.toml"),
    )?)?;
    let host = format!("{}-unknown-linux-gnu", manifest.arch);
    let original = serde_json::json!({
        "schema":1,"runtime":{"version":manifest.version,"root":selected.root,
            "publisher":selected.publisher,"toolkit_sha256":"actual-toolkit"},
        "compiler":"actual-compiler","host":host,"architecture":manifest.arch,
        "profile":"thin-app","ui":"actual.rlib","resources":[],"units":[]
    });
    let interface: Interface = serde_json::from_value(original.clone())?;
    validate_binding(&interface, &selected, &manifest, "actual-compiler", &host)?;
    for field in ["schema", "profile", "compiler", "host", "architecture"] {
        let mut changed = original.clone();
        changed[field] = if field == "schema" {
            2.into()
        } else {
            "different".into()
        };
        let interface = serde_json::from_value(changed)?;
        anyhow::ensure!(
            validate_binding(&interface, &selected, &manifest, "actual-compiler", &host).is_err()
        );
    }
    for field in ["version", "root", "publisher"] {
        let mut changed = original.clone();
        changed["runtime"][field] = "different".into();
        let interface = serde_json::from_value(changed)?;
        anyhow::ensure!(
            validate_binding(&interface, &selected, &manifest, "actual-compiler", &host).is_err()
        );
    }
    Ok(())
}

#[test]
fn compiler_descriptor_read_is_bounded_before_json_allocation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let descriptor = temp.path().join("manifest.json");
    std::fs::write(&descriptor, vec![b' '; 8 * 1024 * 1024 + 1])?;
    anyhow::ensure!(read_interface(&descriptor).is_err());
    Ok(())
}

#[test]
fn authored_compiler_targets_cannot_escape_the_source_snapshot() -> Result<()> {
    for source in [
        "[lib]\npath = '../foreign.rs'",
        "[[bin]]\nname = 'foreign'\npath = '/foreign.rs'",
        "[[example]]\nname = 'foreign'\npath = 'src/../../foreign.rs'",
    ] {
        anyhow::ensure!(validate_source_paths(&toml::from_str(source)?).is_err());
    }
    validate_source_paths(&toml::from_str("[lib]\npath = 'src/lib.rs'")?)?;
    Ok(())
}
