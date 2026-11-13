//! The environment visible to a sandboxed app.
//!
//! The app's environment is meant to be a closed set — a list of variables the launcher
//! puts there, beside the binds that make each mean something.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;
use tilo_test_env::SpawnUnderEnvLock as _;

use launcher::run::{RunOptions, run_with_captured};
use tilo_test_env::ScopedEnv;

use crate::support::{bwrap, isolated_options, manifest, workspacetest_payload};

const PACKAGE: &str = "so.tilo.envtest";

/// A variable name no part of Tilo sets, so its presence inside the sandbox can only mean one thing: it
/// walked in from the parent.
const MARKER: &str = "TILO_ENV_LEAK_MARKER";

/// Stand-ins for compositor wiring that an app must not receive: the privileged clipboard socket,
/// collaboration broker, and user identity file.
const SESSION_PRIVATE: &[(&str, &str)] = &[
    ("TILO_WAYLAND_PRIVILEGED", "wayland-priv-9"),
    ("QUICK_COLLAB_SOCK", "/run/user/1000/tilo-collab.sock"),
    (
        "TILO_USER_IDENTITY",
        "/home/somebody/.config/tilo/identity/primary.json",
    ),
    ("TILO_IDENTITY_BROKER_FD", "42"),
];

/// Variables set on *this* process for the duration of one test, put back afterwards — what the
/// launcher carries when it builds the launch is the whole subject here.
fn parent_env(vars: &[(&str, &str)]) -> ScopedEnv {
    let mut env = ScopedEnv::new();
    for (key, value) in vars {
        env.set(key, value);
    }
    env
}

fn run(argv: &[&str]) -> String {
    let data = tempfile::tempdir().unwrap();
    let opts = RunOptions {
        no_services: true,
        bwrap_bin: Some(bwrap()),
        landlock_enforce: false,
        mode: launcher::run::LaunchMode::Cli {
            command: "workspacetest".to_string(),
            passthrough_argv: argv.iter().map(std::string::ToString::to_string).collect(),
        },
        ..isolated_options(data.path())
    };
    let (status, out) = run_with_captured(
        &manifest(PACKAGE, "bin/workspacetest", "off"),
        workspacetest_payload(),
        &opts,
    )
    .expect("launch");
    let text = String::from_utf8_lossy(&out).trim().to_string();
    assert!(
        status.success(),
        "fixture exited {status:?} with stdout {text:?}"
    );
    text
}

/// Every variable name the sandboxed process has, from its own `environ`.
fn env_names_inside() -> Vec<String> {
    let line = run(&["envall"]);
    let names = line
        .strip_prefix("ENVALL ")
        .unwrap_or_else(|| panic!("fixture did not answer envall: {line:?}"));
    names
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn the_session_environment_does_not_leak_into_the_sandbox() {
    {
        let mut planted: Vec<(&str, &str)> = vec![(MARKER, "the-parent-was-here")];
        planted.extend_from_slice(SESSION_PRIVATE);
        let _env = parent_env(&planted);

        // Without this the test would pass on a launcher that never had the variable to leak in the
        // first place.
        assert_eq!(
            std::env::var(MARKER).as_deref(),
            Ok("the-parent-was-here"),
            "the marker must be set on the launching process, or this test cannot \
             tell a cleared environment from an empty one"
        );

        let inside = env_names_inside();

        assert!(
            !inside.iter().any(|k| k == MARKER),
            "{MARKER} was set on the launching process and reached the sandbox; \
             the app inherits the session's environment. Saw: {inside:?}"
        );
        for (key, _) in SESSION_PRIVATE {
            assert!(
                !inside.iter().any(|k| k == key),
                "{key} describes the session's own wiring and must not be in a \
                 sandboxed app's environment. Saw: {inside:?}"
            );
        }
    }
    {
        let _env = parent_env(&[
            ("EDITOR", "vi"),
            ("SSH_AUTH_SOCK", "/run/user/1000/keyring/ssh"),
            ("XDG_SESSION_TYPE", "wayland"),
            (MARKER, "the-parent-was-here"),
        ]);

        // What the launcher gives an app: `PATH` and the allowlist from `push_clean_environment`, plus
        // one set beside each bind that makes it reachable.
        let mut allowed: Vec<String> = vec![
            "PATH".to_string(),
            // `--clearenv` keeps `PWD`, which is a path inside the sandbox rather than anything of the
            // session's.
            "PWD".to_string(),
            "TILO_PACKAGE_ID".to_string(),
            "TILO_APP_NAME".to_string(),
            "TILO_APP_ICON".to_string(),
            "TILO_APP_DIR".to_string(),
            "TILO_PERSON_DIR".to_string(),
            "TILO_USER_HOME".to_string(),
            "TILO_USER_IDENTITY".to_string(),
            "TILO_IDENTITY_BROKER_FD".to_string(),
            "TILO_WORKSPACE_ID".to_string(),
            "TILO_WORKSPACE_ROOTS".to_string(),
            "TILO_PARTICIPATION".to_string(),
            "TILO_CLI_COMMAND".to_string(),
            // The shared font index's directory, set beside its bind (`appdir::push_font_index`).
            tilo_process::environment::FONT_INDEX_ENV.to_string(),
            // The egress gate and proxy variables naming it.
            "TILO_NET_GATE".to_string(),
            "HTTPS_PROXY".to_string(),
            "HTTP_PROXY".to_string(),
            "ALL_PROXY".to_string(),
            "https_proxy".to_string(),
            "http_proxy".to_string(),
            "all_proxy".to_string(),
            "NO_PROXY".to_string(),
            "no_proxy".to_string(),
            "HOME".to_string(),
            "XDG_DATA_HOME".to_string(),
            "XDG_STATE_HOME".to_string(),
            "XDG_CONFIG_HOME".to_string(),
            "XDG_CACHE_HOME".to_string(),
            "XDG_RUNTIME_DIR".to_string(),
            "WAYLAND_DISPLAY".to_string(),
            "DBUS_SESSION_BUS_ADDRESS".to_string(),
            "LD_LIBRARY_PATH".to_string(),
            "LD_PRELOAD".to_string(),
            "GLIBC_TUNABLES".to_string(),
        ];
        // The loader pin, set beside the read-only bind of the manifests it names: a GPU's for a
        // launch with a render node, lavapipe's for one without when the session pinned it (CI's
        // environment does).
        allowed.extend(
            [
                launcher::gpu::VK_DRIVER_FILES_ENV,
                launcher::gpu::VK_ICD_FILENAMES_ENV,
                launcher::gpu::EGL_VENDOR_ENV,
            ]
            .map(str::to_string),
        );
        allowed.extend(
            launcher::sandbox::INHERITED_ENV
                .iter()
                .map(std::string::ToString::to_string),
        );
        allowed.extend(
            launcher::sandbox::ACCESSIBILITY_TEXT_SCALE_ENV
                .iter()
                .map(std::string::ToString::to_string),
        );

        let inside = env_names_inside();
        assert!(
            !inside.is_empty(),
            "the fixture reported no environment at all, which would make every \
             other assertion here vacuous"
        );

        let unexpected: Vec<&String> = inside
            .iter()
            // Service-socket descriptors are named per service.
            .filter(|k| !allowed.contains(k) && !k.starts_with("TILO_SVC_"))
            .collect();
        assert!(
            unexpected.is_empty(),
            "a sandboxed app's environment must hold only what the launcher put \
             there; these arrived some other way: {unexpected:?}"
        );
    }
    {
        let _env = parent_env(&[("PATH", "/home/somebody/bin:/usr/bin")]);

        let path = run(&["env", "PATH"]);
        assert_eq!(
            path,
            format!("ENV PATH={}", launcher::sandbox::SANDBOX_PATH),
            "a sandboxed app must not inherit the session's PATH"
        );
    }
    {
        for value in ["NaN", "0.49", "4.01", "1.25\nTILO_ENV_LEAK_MARKER=leaked"] {
            let _env = parent_env(&[
                ("TILO_A11Y_TEXT_SCALE", value),
                (MARKER, "the-parent-was-here"),
            ]);
            assert_eq!(
                run(&["env", "TILO_A11Y_TEXT_SCALE"]),
                "ENV TILO_A11Y_TEXT_SCALE=<unset>",
                "unsafe value crossed the sandbox: {value:?}"
            );
            assert!(
                !env_names_inside().iter().any(|key| key == MARKER),
                "unrelated parent environment crossed beside {value:?}"
            );
        }
    }
}

#[test]
fn chosen_session_preferences_are_carried_across_explicitly() {
    {
        let _env = parent_env(&[("LANG", "en_US.UTF-8"), ("LC_ALL", "en_US.UTF-8")]);

        let lang = run(&["env", "LANG"]);
        assert_eq!(
            lang, "ENV LANG=en_US.UTF-8",
            "the session's locale must reach the app; without it every app answers \
             in the C locale"
        );
    }
    {
        let _env = parent_env(&[("RUST_LOG", "info,file_explorer::drag=debug")]);

        let seen = run(&["env", "RUST_LOG"]);
        assert_eq!(
            seen, "ENV RUST_LOG=info,file_explorer::drag=debug",
            "the session's log filter must reach the app; without it every app \
             inside the sandbox is silent below error level"
        );
    }
    {
        let _env = parent_env(&[
            ("TILO_A11Y_TEXT_SCALE", " 1.25 "),
            ("TILO_TEXT_SCALE", "2.0"),
        ]);

        assert_eq!(
            run(&["env", "TILO_A11Y_TEXT_SCALE"]),
            "ENV TILO_A11Y_TEXT_SCALE=1.25"
        );
        assert_eq!(run(&["env", "TILO_TEXT_SCALE"]), "ENV TILO_TEXT_SCALE=2");
    }
}

/// The shared objects `executables` load, which must include `libgstreamer`.
fn native_closure(executables: &[&std::path::Path]) -> Vec<PathBuf> {
    let mut libraries = std::collections::BTreeSet::new();
    for executable in executables {
        let report = std::process::Command::new("ldd")
            .arg(executable)
            .output_locked()
            .unwrap();
        assert!(report.status.success());
        let report = String::from_utf8(report.stdout).unwrap();
        assert!(!report.contains("not found"), "{report}");
        libraries.extend(
            report
                .split_whitespace()
                .filter(|word| word.starts_with('/') && !word.ends_with(':'))
                .map(PathBuf::from),
        );
    }
    assert!(
        libraries.iter().any(|path| path
            .file_name()
            .is_some_and(|name| name == "libgstreamer-1.0.so.0")),
        "{libraries:?}"
    );
    libraries.into_iter().collect()
}

/// The real native library relocates its lookup beside the signed runtime.
#[test]
fn relocated_gstreamer_discovers_and_decodes_in_the_confined_process() {
    use std::path::Path;
    use std::process::Command;

    let inspect = Path::new("/usr/bin/gst-inspect-1.0");
    let launch = Path::new("/usr/bin/gst-launch-1.0");
    assert!(
        inspect.is_file() && launch.is_file(),
        "native GStreamer tools are required"
    );
    let root = tempfile::tempdir().unwrap();
    let payload = root.path().join("payload");
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    // Encoded here, outside the sandbox, with the host's own plugin search.
    let encoded = Command::new(launch)
        .arg("-q")
        .args(
            "videotestsrc num-buffers=10 ! video/x-raw,width=64,height=48 ! vp8enc ! webmmux ! filesink"
                .split(' '),
        )
        .arg(format!("location={}", payload.join("clip.webm").display()))
        .output_locked()
        .unwrap();
    assert!(encoded.status.success(), "{encoded:?}");

    let mut env = ScopedEnv::new();
    env.set("LC_ALL", "C");
    env.set("GST_PLUGIN_SYSTEM_PATH_1_0", "/home/private/plugins");
    let libraries = native_closure(&[inspect, launch]);
    let runtime = root.path().join("runtime");
    let identity = runtime_bundle::Identity::generate();
    let key = root.path().join("fixture-identity.json");
    std::fs::write(&key, identity.to_json_bytes()).unwrap();
    runtime_bundle::build_bundle_with(
        &runtime,
        "gst-relocation-fixture",
        &runtime_bundle::BuildOptions {
            identity: Some(&key),
            extra_libs: &libraries,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!runtime.join("lib/gstreamer-1.0").exists());
    std::fs::copy(inspect, payload.join("bin/gst-inspect")).unwrap();
    std::fs::copy(launch, payload.join("bin/gst-launch")).unwrap();
    let confined = |bin: &str, argv: Vec<String>| {
        let mut manifest = manifest(PACKAGE, "bin/workspacetest", "off");
        manifest.runtime = Some("tilo-runtime/gst-relocation-fixture".into());
        manifest.entrypoint.bin = format!("bin/{bin}");
        let opts = RunOptions {
            no_services: true,
            bwrap_bin: Some(bwrap()),
            sandbox_shim_bin: Some(tilo_test_env::package_binary!("tilo-launcher")),
            runtime_root: Some(runtime.clone()),
            require_runtime_publisher: Some(identity.public_key_hex()),
            mode: launcher::run::LaunchMode::Cli {
                command: bin.into(),
                passthrough_argv: argv,
            },
            ..isolated_options(&root.path().join("data"))
        };
        assert!(opts.landlock_enforce);
        assert!(!opts.allow_missing_runtime && !opts.allow_unsigned_runtime);
        let (status, output) = run_with_captured(&manifest, &payload, &opts)
            .expect("launch the native probe with the verified relocated runtime");
        let output = String::from_utf8_lossy(&output).into_owned();
        assert!(status.success(), "{bin} failed: {status:?} {output}");
        output
    };

    let output = confined("gst-inspect", vec!["playbin".into()]);
    assert!(output.contains("Player Bin 2"), "{output}");
    assert!(output.contains("libgstplayback.so"), "{output}");

    let clip = launcher::sandbox::payload_mount_for(PACKAGE).join("clip.webm");
    let output = confined(
        "gst-launch",
        vec![
            "-v".into(),
            "playbin".into(),
            format!("uri=file://{}", clip.display()),
            "video-sink=fakesink name=frames silent=false".into(),
        ],
    );
    // Only decoded pictures reach playbin's video sink, so a buffer there proves a system decoder was
    // plugged and ran.
    assert!(
        output
            .lines()
            .any(|line| line.contains("last-message = chain") && line.contains("(frames:sink)")),
        "no decoded picture reached the sink: {output}"
    );
}
