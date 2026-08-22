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
//! cargo run -p app-catalogue --example serve-test-registry -- \
//!     --config-dir /tmp/qos-cfg --payload /bin/true
//! ```
//!
//! Writes `registries.toml` into `--config-dir` pointing at itself,
//! prints the port, and serves until killed.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use pkg::pack::{SigningIdentity, pack};
use pkg::{Manifest, MemoryBlobStore};
use registry_types::{RegistryEntry, RegistryVersion, sign_entry};

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() {
    let config_dir = PathBuf::from(arg("--config-dir").expect("--config-dir is required"));
    let package_id = arg("--package-id").unwrap_or_else(|| "dev.qos.sheets".to_string());
    let name = arg("--name").unwrap_or_else(|| "Sheets".to_string());
    // The payload binary the packaged app runs. `/bin/true` proves the
    // install path; point it at a real client to prove the launch.
    let payload_bin = PathBuf::from(arg("--payload").unwrap_or_else(|| "/bin/true".to_string()));

    // Development knobs for observing the slow and failing paths a
    // local loopback registry is otherwise too fast to show.
    let delay_ms: u64 = arg("--delay-ms").and_then(|s| s.parse().ok()).unwrap_or(0);
    let corrupt = std::env::args().any(|a| a == "--corrupt");

    let identity = SigningIdentity::generate();
    let mut qpkg = author(&package_id, &name, &payload_bin, &identity);
    let advertised_len = qpkg.len();
    if corrupt {
        qpkg.truncate(advertised_len / 2);
    }

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{port}");

    let key = SigningKey::from_bytes(&[23u8; 32]);
    let mut entry = RegistryEntry {
        package_id: package_id.clone(),
        name: name.clone(),
        description: format!("{name}, served by the local test registry"),
        categories: vec!["Office".into()],
        icon_url: None,
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
        versions: vec![RegistryVersion {
            version: "1.4.0".into(),
            qpkg_url: format!("{base_url}/app.qpkg"),
            qpkg_root_hash_hex: hex::encode([0u8; 32]),
            qpkg_size_bytes: advertised_len as u64,
            released_at_unix: 1,
        }],
        updated_at_unix: 2,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &key);
    let entries_json = serde_json::to_string(&vec![entry]).expect("encode entries");

    std::fs::create_dir_all(&config_dir).expect("create config dir");
    std::fs::write(
        config_dir.join("registries.toml"),
        format!("[[registry]]\nname = \"local test\"\nbase_url = \"{base_url}\"\n"),
    )
    .expect("write registries.toml");

    println!("registry {base_url} serving {package_id} ({name}) — config in {config_dir:?}");
    // Flush so a harness waiting on stdout sees it immediately.
    let _ = std::io::stdout().flush();

    let entries = Arc::new(entries_json);
    let bytes = Arc::new(qpkg);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        serve_one(stream, &entries, &bytes, delay_ms);
    }
}

fn author(
    package_id: &str,
    name: &str,
    payload_bin: &std::path::Path,
    identity: &SigningIdentity,
) -> Vec<u8> {
    let pubhex = hex::encode(identity.public_key_bytes());
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = dir.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::copy(payload_bin, payload.join("bin").join("app"))
        .unwrap_or_else(|e| panic!("copy {payload_bin:?} into the payload: {e}"));
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(payload.join("bin").join("app"))
            .unwrap()
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(payload.join("bin").join("app"), perms).unwrap();
    }
    let manifest_toml = format!(
        r#"
[package]
id = "{package_id}"
version = "1.4.0"
kind = "app"
name = "{name}"
category = "Office"

[publisher]
identity = "ed25519:{pubhex}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "qos-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#
    );
    let manifest = Manifest::from_toml_str(&manifest_toml).expect("manifest parses");
    let mut store = MemoryBlobStore::new();
    let id = pack(&payload, &manifest, identity, &mut store).expect("pack");
    app_registry::qpkg::write_bytes(id, &store).expect("write qpkg bytes")
}

fn serve_one(mut stream: TcpStream, entries: &str, qpkg: &[u8], delay_ms: u64) {
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
    let (status, ctype, body): (&str, &str, Vec<u8>) = if path.starts_with("/entries") {
        ("200 OK", "application/json", entries.as_bytes().to_vec())
    } else if path.ends_with(".qpkg") {
        ("200 OK", "application/octet-stream", qpkg.to_vec())
    } else {
        ("404 Not Found", "text/plain", b"no".to_vec())
    };
    // Only the package download is slowed; the catalogue listing stays
    // fast so the palette still fills in while you type.
    if delay_ms > 0 && path.ends_with(".qpkg") {
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
    }
    eprintln!("[registry] {} -> {status}", path);
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}
