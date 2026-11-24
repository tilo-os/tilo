//! An app whose entrypoint is a script a runtime's binary runs (`[entrypoint] script`): a
//! Python and a JavaScript one. Each is started by the launcher under `bwrap` exactly as any
//! app, on the signed runtime bundle of its runtime package (`so.tilo.runtime.python`,
//! `so.tilo.runtime.bun`), handed the daemon's native endpoint as a descriptor the launcher
//! connected itself, and reaching the network only through its egress grant.
//!
//! The runtime packages are built from their pinned upstream releases by
//! `tools/apps/runtime_packages.sh`; the `script-runtimes` t check hands their unpacked payloads
//! to these tests in `TILO_RUNTIME_PACKAGES`, as the env-runtime check hands another test a
//! tree. Each test skips when it is absent, and that check runs them and refuses a skip.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use collab_types::caps::{Access, CapOverride, LaunchGrants, Scope};
use launcher::manifest::ServiceReq;
use launcher::run::{LaunchOptions, launch_with_captured};
use tilo_action_policy::Grantee;

use crate::support::{bwrap, isolated_options, manifest};

const PACKAGE: &str = "so.tilo.script-probe";
/// The name the egress grant is written against.
const ALLOWED: &str = "localhost";
/// RFC 2606 reserves `.invalid`, so a refusal for it can only be the policy's.
const BLOCKED: &str = "blocked.invalid";
/// The runtime bundle an app links its own interpreter's libraries against. Any real app names
/// the release it was built for; this test builds the host's minimal set under a name of its own.
const APP_RUNTIME: &str = "test-app-runtime";

/// What the Python probe does, in order, one line of output each: where it runs from, what the
/// daemon answered on the descriptor it was handed, and what each HTTP request through the proxy
/// the sandbox names came back with.
const PYTHON_SCRIPT: &str = r#"
import os, socket, sys, urllib.request

print("SCRIPT " + sys.argv[0])
print("MODULE " + os.environ.get("TILO_APP_MODULE", ""))
print("WORKSPACE " + os.environ.get("TILO_WORKSPACE_ID", ""))
native = socket.socket(fileno=int(os.environ["TILO_SVC_NATIVE_FD"]))
native.sendall(b'{"version":1,"message":{"probe":"hello"}}\n')
print("NATIVE " + native.makefile().readline().strip())
for host in open(os.path.join(os.environ["TILO_APP_MODULE"], "targets")).read().split():
    try:
        with urllib.request.urlopen(host, timeout=10) as answer:
            print("GET " + str(answer.status) + " " + answer.read().decode())
    except Exception as error:
        print("GET refused " + type(error).__name__)
for masked in ("/proc/net/tcp", "/proc/sys/kernel/hostname"):
    try:
        with open(masked) as probe:
            probe.read(1)
        print("MASKED read " + masked)
    except Exception:
        print("MASKED refused " + masked)
"#;

/// [`PYTHON_SCRIPT`]'s work, in JavaScript, with an absolute-form request to the proxy the
/// sandbox names (`HTTP_PROXY`): the same three answers, from the same sandbox.
const JAVASCRIPT_SCRIPT: &str = r#"
import fs from "node:fs";
import net from "node:net";

const mount = process.env.TILO_APP_MODULE ?? "";
console.log("SCRIPT " + process.argv[1]);
console.log("MODULE " + mount);
console.log("WORKSPACE " + (process.env.TILO_WORKSPACE_ID ?? ""));

const native = Number(process.env.TILO_SVC_NATIVE_FD);
fs.writeSync(native, '{"version":1,"message":{"probe":"hello"}}\n');
let frame = "";
const received = Buffer.alloc(4096);
while (!frame.includes("\n")) {
  const read = fs.readSync(native, received, 0, received.length, null);
  if (read === 0) break;
  frame += received.toString("utf8", 0, read);
}
console.log("NATIVE " + frame.trim());
fs.closeSync(native);

function fetchThroughProxy(url) {
  return new Promise((resolve) => {
    const proxy = new URL(process.env.HTTP_PROXY);
    const socket = net.connect(Number(proxy.port), proxy.hostname, () => {
      socket.write(
        `GET ${url} HTTP/1.1\r\nHost: ${new URL(url).host}\r\nConnection: close\r\n\r\n`,
      );
    });
    let answer = "";
    socket.on("data", (chunk) => (answer += chunk.toString()));
    socket.on("error", () => resolve("refused Socket"));
    socket.on("close", () => {
      const head = (answer.split("\r\n")[0] ?? "").split(" ");
      const body = answer.split("\r\n\r\n").slice(1).join("\r\n\r\n").trim();
      resolve(head[1] === "200" ? `200 ${body}` : `refused ${head[1] ?? "closed"}`);
    });
  });
}

const targets = fs.readFileSync(`${mount}/targets`, "utf8").trim().split(/\s+/);
for (const url of targets) {
  console.log("GET " + (await fetchThroughProxy(url)));
}
for (const masked of ["/proc/net/tcp", "/proc/sys/kernel/hostname"]) {
  try {
    fs.readFileSync(masked);
    console.log("MASKED read " + masked);
  } catch {
    console.log("MASKED refused " + masked);
  }
}
process.exit(0);
"#;

/// The runtime package `name` as the `script-runtimes` t check unpacked it: its manifest (what a
/// device reads: the runtime it provides and the binary its entrypoint names), the verified
/// bundle the launcher mounts, and the publisher a device trusts it by.
struct RuntimePackage {
    provides: String,
    bin: String,
    publisher: String,
    bundle: PathBuf,
}

/// [`RuntimePackage`] for `name`, or `None` when no package tree was handed to this run.
fn runtime_package(name: &str) -> Option<RuntimePackage> {
    let root = PathBuf::from(std::env::var_os("TILO_RUNTIME_PACKAGES")?);
    let manifest: toml::Value =
        toml::from_str(&std::fs::read_to_string(root.join(format!("{name}.manifest.toml")))
            .expect("the runtime package's manifest is in the package tree"))
        .expect("the runtime package's manifest is TOML");
    let provides = manifest["package"]["provides"]
        .as_str()
        .expect("a runtime package names what it provides")
        .to_owned();
    let bin = manifest["entrypoint"]["bin"]
        .as_str()
        .expect("a runtime package names the binary its apps run")
        .to_owned();
    let version = provides
        .split_once('/')
        .expect("provides is `<name>/<version>`")
        .1;
    assert!(
        provides.starts_with(&format!("{name}/")),
        "the {name} package provides {provides}, not the {name} runtime"
    );
    let bundle = root.join(version);
    assert!(
        bundle.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME).is_file(),
        "the {name} package's payload is not at {}",
        bundle.display()
    );
    let publisher = std::fs::read_to_string(root.join(format!("{name}.publisher")))
        .expect("the runtime package's publisher is in the package tree");
    Some(RuntimePackage {
        provides,
        bin,
        publisher: publisher.trim().to_owned(),
        bundle,
    })
}

/// Copy `from` into `to`: a launcher sees a runtime package's payload where an install put it.
fn install_bundle(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a runtime bundle directory");
    for entry in std::fs::read_dir(from).expect("the bundle's files") {
        let entry = entry.expect("a bundle entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("a bundle entry's type").is_dir() {
            install_bundle(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("a bundle file");
        }
    }
}

/// A runtime bundle of the host's minimal loader set, signed by `identity`, under [`APP_RUNTIME`]:
/// what an app's `package.runtime` names, whose libraries the interpreter's own binary runs on.
fn app_runtime(data: &Path, identity: &Path) {
    runtime_bundle::build_bundle_with(
        &data.join("tilo-runtime").join(APP_RUNTIME),
        APP_RUNTIME,
        &runtime_bundle::BuildOptions {
            from: None,
            arch: None,
            identity: Some(identity),
            extra_libs: &[],
            extra_bins: &[],
        },
    )
    .expect("the app runtime bundle is built");
}

/// A server answering every request with `reached`, on a loopback port: the host the grant names.
fn server() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\nreached",
            );
        }
    });
    port
}

/// Start `script` under `package`'s runtime, in the app sandbox, and answer what it printed; the
/// daemon's endpoint is listened to here and answered with the peer it saw.
fn probe(package: &str, script_name: &str, script: &str) -> String {
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    home.env().remove(runtime_bundle::RUNTIME_DIR_ENV);
    let root = home.root().to_path_buf();
    let data = home.data().to_path_buf();

    let package = runtime_package(package).expect("the package tree was handed to this run");
    let version = package.provides.split_once('/').expect("provides").1;
    install_bundle(&package.bundle, &data.join("tilo-runtime").join(version));
    let identity = runtime_bundle::Identity::generate();
    let signer = root.join("identity.json");
    std::fs::write(&signer, identity.to_json_bytes()).expect("the identity is kept");
    app_runtime(&data, &signer);
    let trusted = root.join("trusted.txt");
    std::fs::write(
        &trusted,
        format!("{}\n{}\n", identity.public_key_hex(), package.publisher),
    )
    .expect("trusted");
    home.env()
        .set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trusted);

    // The daemon's native endpoint, where the launcher looks for it.
    let runtime_dir = root.join("run");
    let services = runtime_dir.join("tilo/services");
    std::fs::create_dir_all(&services).expect("the services directory");
    home.env().set("XDG_RUNTIME_DIR", &runtime_dir);
    let endpoint = UnixListener::bind(services.join("native.sock")).expect("the native endpoint");
    let daemon = std::thread::spawn(move || {
        let (stream, _) = endpoint.accept().expect("the launcher connects");
        let peer = rustix::net::sockopt::socket_peercred(&stream).expect("the peer's credentials");
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .expect("the app sends a frame");
        let mut stream = stream;
        writeln!(stream, "{{\"peer\":{}}}", peer.pid.as_raw_nonzero()).expect("answered");
        line
    });

    let payload = root.join("payload");
    std::fs::create_dir_all(&payload).expect("the payload");
    std::fs::write(payload.join(script_name), script).expect("the script");

    let mut m = manifest(PACKAGE, &package.bin, "allowlist");
    m.runtime = Some(format!("tilo-runtime/{APP_RUNTIME}"));
    m.sandbox.net.hosts = vec![ALLOWED.to_owned()];
    m.entrypoint.runtime = Some(package.provides.clone());
    m.entrypoint.script = Some(script_name.to_owned());
    m.required_runtimes = vec![package.provides.clone()];
    m.sandbox.services.insert(
        launcher::services::NATIVE_SERVICE.to_owned(),
        ServiceReq::parse("", "required@40").expect("a requirement"),
    );

    // The workspace granted the host the manifest declares, and nothing else.
    let grants = root.join("grants.json");
    let granted = LaunchGrants {
        author: false,
        sandbox: vec![
            CapOverride::grant(
                Grantee::person("ada"),
                Scope::NetHost(ALLOWED.to_owned()),
                Access::Write,
            )
            .through(PACKAGE),
        ]
        .into_iter()
        .collect(),
    };
    std::fs::write(&grants, serde_json::to_vec(&granted).expect("grants")).expect("grants kept");

    let port = server();
    std::fs::write(
        payload.join("targets"),
        format!("http://{ALLOWED}:{port}/ http://{BLOCKED}/"),
    )
    .expect("the targets");
    let opts = LaunchOptions {
        bwrap_bin: Some(bwrap()),
        landlock_enforce: true,
        sandbox_shim_bin: Some(tilo_test_env::package_binary!("tilo-launcher")),
        capabilities_path: Some(grants),
        workspace_id: Some("workspace-a".to_owned()),
        ..isolated_options(&data)
    };
    let (status, out) = launch_with_captured(&m, &payload, &opts).expect("the launch");
    let out = String::from_utf8_lossy(&out).into_owned();
    assert!(status.success(), "the script exited {status:?}: {out}");

    let sent = daemon.join().expect("the endpoint answered");
    assert!(
        sent.contains("\"probe\":\"hello\""),
        "the frame arrived: {sent}"
    );

    let said = |prefix: &str| -> Vec<String> {
        out.lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .map(str::to_owned)
            .collect()
    };
    let mount = launcher::sandbox::payload_mount_for(PACKAGE);
    assert_eq!(
        said("SCRIPT "),
        vec![mount.join(script_name).display().to_string()],
        "the runtime's binary runs the package's script, where the sandbox mounts it: {out}"
    );
    assert_eq!(said("MODULE "), vec![mount.display().to_string()], "{out}");
    assert_eq!(said("WORKSPACE "), vec!["workspace-a".to_owned()], "{out}");
    assert_eq!(
        said("NATIVE "),
        vec![format!("{{\"peer\":{}}}", std::process::id())],
        "the connection's peer is the launching process, the one a daemon records: {out}"
    );
    assert_eq!(
        said("GET ").first(),
        Some(&"200 reached".to_owned()),
        "the host the grant names answers: {out}"
    );
    assert!(
        said("GET ").get(1).is_some_and(|line| line.starts_with("refused ")),
        "any other host is refused: {out}"
    );
    assert_eq!(said("GET ").len(), 2, "{out}");
    assert_eq!(
        said("MASKED "),
        vec![
            "refused /proc/net/tcp".to_owned(),
            "refused /proc/sys/kernel/hostname".to_owned(),
        ],
        "the sandbox's procfs keeps its sockets and tunables from the runtime: {out}"
    );
    out
}

/// A packaged Python script starts in the app sandbox on the Python runtime package's binary,
/// finds its script and its package where the sandbox mounts them, and is handed the daemon's
/// native endpoint already connected by the launcher, so the connection's peer is the process the
/// daemon recorded for the launch: what admits it as the app. The network it reaches is its
/// grant's alone: the host the grant names answers, and any other is refused.
#[test]
fn a_python_script_runs_sandboxed_as_its_app_and_reaches_only_its_grant() {
    if runtime_package("python").is_none() {
        eprintln!("skipping: TILO_RUNTIME_PACKAGES names no runtime package tree");
        return;
    }
    probe("python", "main.py", PYTHON_SCRIPT);
}

/// [`a_python_script_runs_sandboxed_as_its_app_and_reaches_only_its_grant`] for a JavaScript
/// program run by the bun runtime package's binary.
#[test]
fn a_javascript_script_runs_sandboxed_as_its_app_and_reaches_only_its_grant() {
    if runtime_package("bun").is_none() {
        eprintln!("skipping: TILO_RUNTIME_PACKAGES names no runtime package tree");
        return;
    }
    probe("bun", "main.js", JAVASCRIPT_SCRIPT);
}
