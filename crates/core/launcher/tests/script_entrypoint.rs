//! An app whose entrypoint is a script a runtime's binary runs (`[entrypoint] script`), here a
//! Python one: started by the launcher under `bwrap` exactly as any app, on a signed runtime
//! bundle, handed the daemon's native endpoint as a descriptor the launcher connected itself, and
//! reaching the network only through its egress grant.

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
/// The Python runtime the probe names: a version with its name after `+`, as every runtime bundle
/// beside another's is, since bundles are installed by version.
const PYTHON: &str = "3-probe+python";
/// The name the egress grant is written against.
const ALLOWED: &str = "localhost";
/// RFC 2606 reserves `.invalid`, so a refusal for it can only be the policy's.
const BLOCKED: &str = "blocked.invalid";

/// What the probe does, in order, one line of output each: where it runs from, what the daemon
/// answered on the descriptor it was handed, and what each HTTP request through the proxy the
/// sandbox names came back with.
const SCRIPT: &str = r#"
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
"#;

/// A runtime bundle `version` under `data`'s runtimes holding `bins`, signed by `identity`.
fn bundle(data: &Path, version: &str, identity: &Path, bins: &[PathBuf]) {
    runtime_bundle::build_bundle_with(
        &data.join("tilo-runtime").join(version),
        version,
        &runtime_bundle::BuildOptions {
            from: None,
            arch: None,
            identity: Some(identity),
            extra_libs: &[],
            extra_bins: bins,
        },
    )
    .expect("the runtime bundle is built");
}

/// The build image's Python, the binary the probe's runtime bundle carries.
fn python() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
        .map(|found| std::fs::canonicalize(found).expect("python3 resolves"))
        .expect("python3 is on the build image's PATH")
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

/// A packaged Python script starts in the app sandbox on its runtime's binary, finds its
/// script and its package where the sandbox mounts them, and is handed the daemon's native
/// endpoint already connected by the launcher, so the connection's peer is the process the
/// daemon recorded for the launch: what admits it as the app. The network it reaches is its
/// grant's alone: the host the grant names answers, and any other is refused.
#[test]
fn a_python_script_runs_sandboxed_as_its_app_and_reaches_only_its_grant() {
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    home.env().remove(runtime_bundle::RUNTIME_DIR_ENV);
    let root = home.root().to_path_buf();
    let data = home.data().to_path_buf();

    let identity = runtime_bundle::Identity::generate();
    let signer = root.join("identity.json");
    std::fs::write(&signer, identity.to_json_bytes()).expect("the identity is kept");
    // Under the name the manifest runs: the image's `python3` is a link to a versioned binary.
    let interpreter = root.join("interpreter").join("python3");
    std::fs::create_dir_all(interpreter.parent().expect("a directory")).expect("its directory");
    std::fs::copy(python(), &interpreter).expect("the interpreter");
    bundle(&data, PYTHON, &signer, &[interpreter]);
    let trusted = root.join("trusted.txt");
    std::fs::write(&trusted, format!("{}\n", identity.public_key_hex())).expect("trusted");
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
    std::fs::write(payload.join("main.py"), SCRIPT).expect("the script");

    let mut m = manifest(PACKAGE, "bin/python3", "allowlist");
    m.sandbox.net.hosts = vec![ALLOWED.to_owned()];
    m.entrypoint.runtime = Some(format!("python/{PYTHON}"));
    m.entrypoint.script = Some("main.py".to_owned());
    m.required_runtimes = vec![format!("python/{PYTHON}")];
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
        ],
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
    let said = |prefix: &str| -> Vec<String> {
        out.lines()
            .filter_map(|line| line.strip_prefix(prefix))
            .map(str::to_owned)
            .collect()
    };

    let mount = launcher::sandbox::payload_mount_for(PACKAGE);
    assert_eq!(
        said("SCRIPT "),
        vec![mount.join("main.py").display().to_string()],
        "the runtime's binary runs the package's script, where the sandbox mounts it: {out}"
    );
    assert_eq!(said("MODULE "), vec![mount.display().to_string()], "{out}");
    assert_eq!(said("WORKSPACE "), vec!["workspace-a".to_owned()], "{out}");

    let sent = daemon.join().expect("the endpoint answered");
    assert!(sent.contains("\"probe\":\"hello\""), "the frame arrived: {sent}");
    assert_eq!(
        said("NATIVE "),
        vec![format!("{{\"peer\":{}}}", std::process::id())],
        "the connection's peer is the launching process, the one a daemon records: {out}"
    );

    assert_eq!(
        said("GET "),
        vec![
            "200 reached".to_owned(),
            "refused HTTPError".to_owned(),
        ],
        "the granted host answers and any other is refused: {out}"
    );
}
