//! What a tilolang process reaches: the host the device starts for a tilolang app's instances and
//! for each agent task's programs (`tilo-launcher tilolang-process`), started here under `bwrap`
//! on signed runtime bundles as the device starts it, with a probe in the host's place that
//! answers on its link what each read and write it was asked to make answered.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use launcher::run::LaunchOptions;
use launcher::run::tilolang_process::{ProcessLaunch, STORAGE_MOUNT, launch_tilolang_process};

use crate::support::{bwrap, hostprobe_payload, isolated_options, native_closure};

/// The tilolang runtime the probe ships in, and the Tilo runtime it runs on.
const TILOLANG: &str = "0.1.0-probe";
const TILO: &str = "2026.10.0-probe";

/// A runtime bundle `version` under `data`'s runtimes, of this machine's loader, the libraries
/// `libs` and `bins`, signed by the identity at `identity`.
fn bundle(data: &Path, version: &str, identity: &Path, libs: &[PathBuf], bins: &[PathBuf]) {
    runtime_bundle::build_bundle_with(
        &data.join("tilo-runtime").join(version),
        version,
        &runtime_bundle::BuildOptions {
            from: None,
            arch: None,
            identity: Some(identity),
            extra_libs: libs,
            extra_bins: bins,
        },
    )
    .expect("the runtime bundle is built");
}

/// A tilolang process writes its storage and nothing else, and is refused what it was not
/// granted: a file of this machine's beside its storage, which it can neither read nor write.
/// That is all an agent task's host is granted too: its own storage.
#[test]
fn a_tilolang_process_writes_its_storage_and_is_refused_what_it_was_not_granted() {
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    home.env().remove(runtime_bundle::RUNTIME_DIR_ENV);
    let root = home.root().to_path_buf();
    let data = home.data().to_path_buf();

    let identity = runtime_bundle::Identity::generate();
    let signer = root.join("identity.json");
    std::fs::write(&signer, identity.to_json_bytes()).expect("the identity is kept");
    let probe = root.join("probe").join("tilolang-process");
    std::fs::create_dir_all(probe.parent().expect("a directory")).expect("the probe's directory");
    std::fs::copy(hostprobe_payload().join("bin/hostprobe"), &probe).expect("the probe");
    // The libraries the probe and the Landlock shim load, which the Tilo runtime carries.
    let shim = tilo_test_env::package_binary!("tilo-launcher");
    let libs = native_closure(&[&probe, &shim]);
    bundle(&data, TILOLANG, &signer, &[], &[probe]);
    bundle(&data, TILO, &signer, &libs, &[]);
    let trusted = root.join("trusted.txt");
    std::fs::write(&trusted, format!("{}\n", identity.public_key_hex())).expect("trusted");
    home.env()
        .set("TILO_LAUNCHER_TRUSTED_PUBLISHERS_FILE", &trusted);

    let storage = root.join("storage");
    std::fs::create_dir_all(&storage).expect("the storage");
    // What it was not granted: another directory of this machine's beside its storage.
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    let secret = elsewhere.join("secret");
    std::fs::write(&secret, "kept").expect("a file it was not granted");
    let planted = elsewhere.join("planted");
    // A service socket beside the sandbox's storage, as the daemon's are beside an app's: the
    // host reaches no socket by path, only the descriptors it was handed.
    let service = elsewhere.join("native.sock");
    let listening = std::os::unix::net::UnixListener::bind(&service).expect("the service socket");

    let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("a link");
    let launch = ProcessLaunch {
        runtime: format!("tilolang/{TILOLANG}"),
        app_runtime: format!("tilo-runtime/{TILO}"),
        storage: storage.clone(),
    };
    let opts = LaunchOptions {
        bwrap_bin: Some(bwrap()),
        sandbox_shim_bin: Some(shim),
        ..isolated_options(&data)
    };
    let host =
        std::thread::spawn(move || launch_tilolang_process(&launch, theirs.into(), None, &opts));

    let mut answers = std::io::BufReader::new(ours.try_clone().expect("the link's other half"));
    let mut asks = ours;
    let mut ask = move |line: String| {
        writeln!(asks, "{line}").expect("the host is asked");
        let mut answer = String::new();
        answers.read_line(&mut answer).expect("the host answers");
        answer.trim().to_owned()
    };
    assert_eq!(
        ask(format!("write {STORAGE_MOUNT}/compiled")),
        "OK",
        "it writes its storage"
    );
    let read = ask(format!("read {}", secret.display()));
    assert!(
        read.starts_with("ERR"),
        "it reads nothing it was not granted: {read}"
    );
    let wrote = ask(format!("write {}", planted.display()));
    assert!(
        wrote.starts_with("ERR"),
        "it writes nothing it was not granted: {wrote}"
    );
    // A socket it was handed no descriptor for is out of reach: the daemon's own sockets are
    // reached through the descriptors a launch hands down, never by a path.
    let connected = ask(format!("connect {}", service.display()));
    assert!(
        connected.starts_with("ERR"),
        "it connects to no service socket it was handed no descriptor for: {connected}"
    );
    drop(listening);
    // Closing the link ends the host.
    drop(ask);

    let status = host
        .join()
        .expect("the launch returned")
        .expect("the host started");
    assert!(
        status.success(),
        "the host ended once its link closed: {status:?}"
    );
    assert!(
        storage.join("compiled").is_file(),
        "what it wrote is in its storage"
    );
    assert!(!planted.exists(), "nothing it was not granted was written");
}
