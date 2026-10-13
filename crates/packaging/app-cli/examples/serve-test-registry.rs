//! A real, signed, self-hosted registry you can point a running
//! desktop at.
//!
//! Development tooling, not shipped: it exists so the install-on-open
//! path can be exercised against a live compositor on a machine that
//! has no registry to talk to. Everything it serves is genuinely
//! signed and genuinely verified by the client — the only thing that
//! is a fixture is the app itself.
//!
//! ```text
//! cargo run -p app-cli --example serve-test-registry -- \
//!     --config-dir /tmp/tilo-cfg --payload /bin/true
//! ```
//!
//! Writes `registries.toml` into `--config-dir` pointing at itself,
//! prints the port, and serves until killed.
//!
//! Point `--payload` at a binary inside an installed package's payload
//! root and the package's own `manifest.toml` is what gets published —
//! see [`authoring`] for why serving a synthesised one instead ships an
//! app that cannot collaborate. `--manifest` names one explicitly.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use registry_types::{
    PublisherAuthorization, RegistryEntry, RegistryVersion, SignedRegistrySnapshot, sign_entry,
    sign_snapshot,
};
use tilo_app_package::pack::SigningIdentity;

#[path = "support/authoring.rs"]
mod authoring;

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| i.checked_add(1))
        .and_then(|i| args.get(i))
        .cloned()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the example keeps its linear fixture setup visible as one auditable flow"
)]
fn run() -> Result<(), String> {
    let config_dir = PathBuf::from(arg("--config-dir").ok_or("--config-dir is required")?);
    let package_id = arg("--package-id").unwrap_or_else(|| "so.tilo.sheets".to_string());
    let name = arg("--name").unwrap_or_else(|| "Sheets".to_string());
    // The payload binary the packaged app runs. `/bin/true` proves the
    // install path; point it at a real client to prove the launch.
    let payload_bin = PathBuf::from(arg("--payload").unwrap_or_else(|| "/bin/true".to_string()));

    // Development knobs for observing the slow and failing paths a
    // local loopback registry is otherwise too fast to show.
    let delay_ms: u64 = arg("--delay-ms").and_then(|s| s.parse().ok()).unwrap_or(0);
    let corrupt = std::env::args().any(|a| a == "--corrupt");

    let key = SigningKey::from_bytes(&[23u8; 32]);
    let identity = SigningIdentity::from_signing_key(key.clone());
    let authored = authoring::author(
        &package_id,
        &name,
        &payload_bin,
        arg("--manifest").map(PathBuf::from).as_deref(),
        &identity,
    )
    .map_err(|e| e.to_string())?;
    match &authored.source {
        authoring::ManifestSource::Package(path) => {
            println!("publishing the package manifest at {}", path.display());
        }
        authoring::ManifestSource::Synthesised => {
            println!(
                "no manifest.toml found beside {} — publishing a synthesised one. \
                 It declares no services, so the installed app will run without \
                 collaboration.",
                payload_bin.display()
            );
        }
    }
    let version = authored.version.clone();
    let tapp_root = app_registry::tapp::load(&authored.tapp)
        .map_err(|e| format!("read authored tapp root: {e}"))?
        .package_id
        .to_hex();
    let mut tapp = authored.tapp;
    let advertised_len = tapp.len();
    if corrupt {
        tapp.truncate(advertised_len / 2);
    }

    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind loopback: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("read the bound loopback address: {e}"))?
        .port();
    let base_url = format!("http://127.0.0.1:{port}");

    let mut entry = RegistryEntry {
        package_id: package_id.clone(),
        kind: registry_types::EntryKind::App,
        name: name.clone().into(),
        description: format!("{name}, served by the local test registry").into(),
        tagline: "Native Tilo OS app".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Office".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            // What the package says it is, not a number chosen here —
            // the entry and the manifest have to agree.
            version,
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: format!("{base_url}/app.tapp"),
                tapp_root_hash_hex: tapp_root,
                tapp_size_bytes: advertised_len as u64,
                tapp_blake3_hex: blake3::hash(&tapp).to_hex().to_string(),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &key);
    let owner = SigningKey::from_bytes(&[24; 32]);
    let now = tilo_clock::unix_now();
    let mut snapshot = SignedRegistrySnapshot {
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
    sign_snapshot(&mut snapshot, &owner).map_err(|e| format!("sign snapshot: {e}"))?;
    let entries_json =
        serde_json::to_string(&snapshot).map_err(|e| format!("encode snapshot: {e}"))?;

    std::fs::create_dir_all(&config_dir).map_err(|e| format!("create config dir: {e}"))?;
    std::fs::write(
        config_dir.join("registries.toml"),
        format!(
            "[[registry]]\nname = \"local test\"\nbase_url = \"{base_url}\"\nowner_pubkey_hex = \"{}\"\n",
            hex::encode(owner.verifying_key().to_bytes())
        ),
    )
    .map_err(|e| format!("write registries.toml: {e}"))?;

    println!(
        "registry {base_url} serving {package_id} ({name}) — config in {}",
        config_dir.display()
    );
    // Flush so a harness waiting on stdout sees it immediately.
    let _ = std::io::stdout().flush();

    let entries = Arc::new(entries_json);
    let bytes = Arc::new(tapp);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        serve_one(stream, &entries, &bytes, delay_ms);
    }
    Ok(())
}

fn serve_one(mut stream: TcpStream, entries: &str, tapp: &[u8], delay_ms: u64) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
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
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let is_tapp = std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tapp"));
    let (status, ctype, body): (&str, &str, Vec<u8>) = if path.starts_with("/entries") {
        ("200 OK", "application/json", entries.as_bytes().to_vec())
    } else if is_tapp {
        ("200 OK", "application/octet-stream", tapp.to_vec())
    } else {
        ("404 Not Found", "text/plain", b"no".to_vec())
    };
    // Only the package download is slowed; the catalogue listing stays
    // fast so the palette still fills in while you type.
    if delay_ms > 0 && is_tapp {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
    eprintln!("[registry] {path} -> {status}");
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}
