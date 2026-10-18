//! `tilo-test-registry` — a signed app registry on loopback that offers
//! newer versions of an app this machine already has installed.
//!
//! ```text
//! tilo-test-registry --app so.tilo.settings --version 9999.0.0 --version 9998.0.0
//! ```
//!
//! Each version is the installed copy's own payload and manifest, packed
//! again under that version number and signed by a publisher key made for
//! this run; the registry snapshot is signed by an owner key made for this
//! run too. Nothing is a stand-in: `app repo add <name> <url> --owner-key
//! <key>` pins the owner exactly as for the real registry, and `app update`,
//! `app rollback` and `tilo-install` verify every signature they would for
//! it. Only the registry's location and keys are a fixture.
//!
//! `--incompatible <v>` offers version `v` too, built against a version of
//! the system interface this device does not serve (`system@N+1`, where `N`
//! is the newest it declares): the build a device must neither be offered
//! nor install, and must not run over its image's copy.
//!
//! It prints one JSON line — `{"url", "owner_key", "publisher_key",
//! "versions"}` — once it is serving, then serves until killed. Dev images
//! ship it for the booted-image check that the system image's apps update
//! and roll back through the registry like any other.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::ExitCode;

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryTarget, RegistryVersion, SignedRegistrySnapshot,
};
use tilo_app_package::{Manifest, SigningIdentity};

const USAGE: &str = "usage: tilo-test-registry --app <package-id> --version <v> [--version <v>...] \
     [--incompatible <v>...] [--port <n>]";

struct Args {
    app: String,
    versions: Vec<String>,
    incompatible: Vec<String>,
    port: u16,
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut app = None;
    let mut versions = Vec::new();
    let mut incompatible = Vec::new();
    let mut port = 0;
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--app" => app = Some(value()?),
            "--version" => versions.push(value()?),
            "--incompatible" => incompatible.push(value()?),
            "--port" => port = value()?.parse().map_err(|e| format!("--port: {e}"))?,
            "-h" | "--help" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n{USAGE}")),
        }
    }
    let app = app.ok_or_else(|| format!("--app is required\n{USAGE}"))?;
    if versions.is_empty() {
        return Err(format!("at least one --version is required\n{USAGE}"));
    }
    Ok(Args {
        app,
        versions,
        incompatible,
        port,
    })
}

fn main() -> ExitCode {
    match parse_args(std::env::args().skip(1)).and_then(|args| run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("tilo-test-registry: {error}");
            ExitCode::FAILURE
        }
    }
}

fn random_key() -> Result<SigningKey, String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no randomness for a signing key: {e}"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// One packed version, ready to serve.
struct Packed {
    version: String,
    file: String,
    root_hex: String,
    bytes: Vec<u8>,
    /// The system interfaces its manifest requires, in wire form.
    interfaces: Vec<String>,
}

/// A system interface version this device does not serve: one past the
/// newest it declares.
fn unserved_system_version() -> u32 {
    package_store::interfaces::system_interfaces()
        .and_then(|provided| provided.version(tilo_app_package::interfaces::SYSTEM_INTERFACE))
        .unwrap_or(1)
        .saturating_add(1)
}

/// The installed copy's manifest and payload packed again as `version`,
/// signed by `publisher`.
fn repack(
    manifest_path: &Path,
    payload_root: &Path,
    version: &str,
    requires_system: Option<u32>,
    publisher: &SigningKey,
) -> Result<(Manifest, Packed), String> {
    let source = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let mut manifest = Manifest::from_toml_str(&source)
        .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
    version.clone_into(&mut manifest.package.version);
    if let Some(system) = requires_system {
        let name = tilo_app_package::interfaces::SYSTEM_INTERFACE.to_owned();
        manifest.sandbox.services.insert(
            name.clone(),
            tilo_app_package::manifest::ServiceReq {
                protocol: name,
                requirement: tilo_app_package::manifest::Requirement::Required,
                version: system,
            },
        );
    }
    manifest.publisher.identity = format!(
        "ed25519:{}",
        hex::encode(publisher.verifying_key().to_bytes())
    );
    manifest.publisher.signature.clear();
    let interfaces = manifest.required_interface_wire();
    let identity = SigningIdentity::from_signing_key(publisher.clone());
    let packed = tilo_app_package::pack(payload_root, &manifest, &identity)
        .map_err(|e| format!("pack {version}: {e}"))?;
    let bytes = tilo_app_package::tapp::write(Vec::new(), &packed.meta, Some(&packed))
        .map_err(|e| format!("write {version}: {e}"))?;
    Ok((
        manifest,
        Packed {
            version: version.to_owned(),
            file: format!("/{}-{version}.tapp", packed.meta.manifest.package.id),
            root_hex: packed.id().to_hex(),
            bytes,
            interfaces,
        },
    ))
}

fn run(args: &Args) -> Result<(), String> {
    let installed = package_store::find_installed(&args.app)
        .map_err(|e| format!("read the installed apps: {e}"))?
        .ok_or_else(|| format!("{} is not installed", args.app))?;
    let publisher = random_key()?;
    let owner = random_key()?;

    let listener =
        TcpListener::bind(("127.0.0.1", args.port)).map_err(|e| format!("bind loopback: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("read the bound address: {e}"))?
        .port();
    let base_url = format!("http://127.0.0.1:{port}");

    let mut packed = Vec::new();
    let mut manifest = None;
    let unserved = unserved_system_version();
    let builds = args.versions.iter().map(|version| (version, None)).chain(
        args.incompatible
            .iter()
            .map(|version| (version, Some(unserved))),
    );
    for (version, requires_system) in builds {
        let (m, p) = repack(
            &installed.manifest_path,
            &installed.payload_root,
            version,
            requires_system,
            &publisher,
        )?;
        if requires_system.is_none() {
            manifest.get_or_insert(m);
        }
        packed.push(p);
    }
    let manifest = manifest.ok_or("no version was packed")?;
    // Versions are listed newest first.
    packed.sort_by(|a, b| tilo_app_package::version::compare(&b.version, &a.version));

    let entry = signed_entry(&args.app, &manifest, &packed, &base_url, &publisher);
    let routes = routes(entry, &packed, &owner)?;

    let ready = serde_json::json!({
        "url": base_url,
        "owner_key": hex::encode(owner.verifying_key().to_bytes()),
        "publisher_key": hex::encode(publisher.verifying_key().to_bytes()),
        "versions": packed.iter().map(|p| p.version.clone()).collect::<Vec<_>>(),
        "incompatible": args.incompatible,
        "interfaces": packed
            .iter()
            .map(|p| (p.version.clone(), p.interfaces.clone()))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "packed_from": installed.payload_root,
    });
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{ready}")
        .and_then(|()| stdout.flush())
        .map_err(|e| format!("announce: {e}"))?;

    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        serve(stream, &routes);
    }
    Ok(())
}

/// The registry entry offering every packed version, signed by `publisher`.
fn signed_entry(
    app: &str,
    manifest: &Manifest,
    packed: &[Packed],
    base_url: &str,
    publisher: &SigningKey,
) -> RegistryEntry {
    let name = manifest
        .package
        .name
        .clone()
        .unwrap_or_else(|| package_store::display_name_for_package_id(app));
    let mut entry = RegistryEntry {
        package_id: app.to_owned(),
        kind: registry_types::EntryKind::App,
        name: name.as_str().into(),
        description: format!("{name}, offered by a test registry").into(),
        tagline: name.as_str().into(),
        // Store artwork must be served over https, which a loopback registry
        // is not; the Store draws its own mark for an entry without any.
        images: registry_types::LocalizedImages::default(),
        supported_languages: vec!["en".into()],
        categories: manifest.package.category.clone().into_iter().collect(),
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: packed
            .iter()
            .map(|p| RegistryVersion {
                version: p.version.clone(),
                released_at_unix: 1,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: format!("{base_url}{}", p.file),
                    tapp_root_hash_hex: p.root_hex.clone(),
                    tapp_size_bytes: p.bytes.len() as u64,
                    tapp_blake3_hex: blake3::hash(&p.bytes).to_hex().to_string(),
                    target: RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: std::env::consts::ARCH.into(),
                        isa: std::env::consts::ARCH.into(),
                        runtime: manifest.package.runtime.clone(),
                        interfaces: p.interfaces.clone(),
                    },
                }),
            })
            .collect(),
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    registry_types::sign_entry(&mut entry, publisher);
    entry
}

/// What the registry serves: both snapshot forms signed by `owner`, the
/// version indexes the compact one names, and the packages.
fn routes(
    entry: RegistryEntry,
    packed: &[Packed],
    owner: &SigningKey,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let now = tilo_clock::unix_now();
    let mut full = SignedRegistrySnapshot {
        protocol: registry_types::SNAPSHOT_PROTOCOL,
        sequence: 1,
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(86_400),
        previous_snapshot_hash_hex: String::new(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: entry.publisher_pubkey_hex.clone(),
            can_add_new: true,
        }],
        entries: vec![entry],
        owner_signature_hex: String::new(),
        channel_authorization: None,
        bots_signature_hex: String::new(),
    };
    registry_types::sign_snapshot(&mut full, owner).map_err(|e| format!("sign snapshot: {e}"))?;
    let (mut compact, indexes) =
        registry_types::compact_snapshot(&full).map_err(|e| format!("compact snapshot: {e}"))?;
    registry_types::sign_compact_snapshot(&mut compact, owner)
        .map_err(|e| format!("sign compact snapshot: {e}"))?;

    let mut routes: HashMap<String, Vec<u8>> = HashMap::new();
    routes.insert("/entries".into(), json(&full)?);
    routes.insert("/entries?format=compact".into(), json(&compact)?);
    for index in indexes {
        routes.insert(format!("/indexes/{}.json", index.blake3_hex), index.bytes);
    }
    for p in packed {
        routes.insert(p.file.clone(), p.bytes.clone());
    }
    Ok(routes)
}

fn json(value: &impl serde::Serialize) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|e| format!("encode: {e}"))
}

fn serve(mut stream: TcpStream, routes: &HashMap<String, Vec<u8>>) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.trim().is_empty() => break,
            Ok(_) => {}
            Err(_) => return,
        }
    }
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let (status, body) = match routes.get(target) {
        Some(body) => ("200 OK", body.as_slice()),
        None => ("404 Not Found", b"not found".as_slice()),
    };
    eprintln!("tilo-test-registry: {target} -> {status}");
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream
        .write_all(header.as_bytes())
        .and_then(|()| stream.write_all(body))
        .and_then(|()| stream.flush());
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn arguments_name_the_app_and_its_versions() {
        let args = parse_args(
            [
                "--app",
                "so.tilo.settings",
                "--version",
                "2.0.0",
                "--version",
                "1.0.0",
                "--incompatible",
                "3.0.0",
            ]
            .map(String::from)
            .into_iter(),
        )
        .unwrap();
        assert_eq!(args.app, "so.tilo.settings");
        assert_eq!(args.versions, ["2.0.0", "1.0.0"]);
        assert_eq!(args.incompatible, ["3.0.0"]);
        assert_eq!(args.port, 0);
        assert!(parse_args(["--app", "x"].map(String::from).into_iter()).is_err());
        assert!(parse_args(["--version", "1"].map(String::from).into_iter()).is_err());
    }

    /// What a device's client checks of every entry before it believes a
    /// snapshot, so an entry it would refuse never leaves this registry.
    #[test]
    fn the_signed_entry_passes_the_clients_entry_checks() {
        let manifest = Manifest::from_toml_str(
            r#"
[package]
id = "so.tilo.settings"
name = "Personal Settings"
category = "System"
version = "1.0.0"
kind = "app"
runtime = "tilo-runtime/2026.07"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
system = "required@1"
"#,
        )
        .unwrap();
        let packed = [Packed {
            version: "9999.0.0".into(),
            file: "/so.tilo.settings-9999.0.0.tapp".into(),
            root_hex: "ab".repeat(32),
            bytes: vec![1, 2, 3],
            interfaces: manifest.required_interface_wire(),
        }];
        let publisher = SigningKey::from_bytes(&[7; 32]);
        let entry = signed_entry(
            "so.tilo.settings",
            &manifest,
            &packed,
            "http://127.0.0.1:4000",
            &publisher,
        );
        assert_eq!(entry.validate(), Ok(()));
        assert_eq!(entry.versions[0].target().unwrap().interfaces, ["system@1"]);
    }
}
