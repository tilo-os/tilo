//! A program built for Android as `tilolang build --platform android` builds the app's library
//! holds more thread-locals than bionic has pthread keys. rustc's own Android std gives each
//! `thread_local!` a key, and bionic has about 128: the phone app, with several hundred in its
//! tree, aborted with "out of TLS keys" in its first redraw. The library's std is built from source
//! with native thread-locals instead (`android::native_tls_cargo_args`).
//!
//! The program is linked statically against bionic, so this Linux host's kernel runs it without
//! Android's loader; the same program built against rustc's stock Android std shows the abort
//! the build avoids. It needs the NDK and std's sources, and std's dependencies from crates.io:
//! the `android-tls` check runs this program in the builder image.
//!
//! It builds the two probes itself, with target directories of their own, so it adds no second
//! build of the workspace to a test run: the check starts it, and it is the check's program.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a check that cannot build its probes fails here, and that is its result"
)]

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use tilo_app_builder::android::{Toolchain, native_tls_cargo_args};

const TRIPLE: &str = "x86_64-linux-android";
/// Well past bionic's `PTHREAD_KEYS_MAX` of 128, half of them with a destructor.
const THREAD_LOCALS: usize = 300;

fn probe_source() -> String {
    let mut source = String::from("use std::cell::{Cell, RefCell};\n");
    for index in 0..THREAD_LOCALS / 2 {
        writeln!(
            source,
            "thread_local! {{ static C{index}: Cell<u64> = const {{ Cell::new(0) }}; }}\n\
             thread_local! {{ static S{index}: RefCell<String> = RefCell::new(String::new()); }}"
        )
        .unwrap();
    }
    source.push_str("fn touch() -> usize {\n    let mut touched = 0;\n");
    for index in 0..THREAD_LOCALS / 2 {
        writeln!(
            source,
            "    C{index}.with(|c| {{ c.set(c.get() + 1); touched += c.get() as usize; }});\n    \
             S{index}.with(|s| {{ s.borrow_mut().push('x'); touched += s.borrow().len(); }});"
        )
        .unwrap();
    }
    source.push_str(
        "    touched\n}\n\
         fn main() {\n    \
             let here = touch();\n    \
             let there = std::thread::spawn(touch).join().unwrap();\n    \
             println!(\"touched {here} and {there}\");\n}\n",
    );
    source
}

/// Build the probe with `cargo_args` before Cargo's subcommand, and run it.
fn build_and_run(probe: &Path, target_dir: &Path, cargo_args: &[OsString]) -> std::process::Output {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let env = Toolchain::discover().cargo_env(TRIPLE).unwrap();
    let linker_var = "CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER";
    let linker = env
        .iter()
        .find(|(key, _)| key == linker_var)
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os(linker_var))
        .expect("the NDK's clang links for Android");
    // A static bionic needs compiler-rt's builtins (its string functions pick an implementation
    // by CPU), which rustc's `-nodefaultlibs` leaves out.
    let builtins = Command::new(&linker)
        .arg("-print-libgcc-file-name")
        .output()
        .unwrap();
    assert!(builtins.status.success());
    let builtins = String::from_utf8(builtins.stdout).unwrap();
    let status = Command::new(&cargo)
        .args(cargo_args)
        // Linked statically, the program needs no Android loader to run here. Cargo joins these
        // flags with the ones `cargo_args` sets for the target.
        .arg("--config")
        .arg(format!(
            "target.{TRIPLE}.rustflags=[\"-Ctarget-feature=+crt-static\", \"-Clink-arg={}\"]",
            builtins.trim()
        ))
        .args(["build", "--release", "--target", TRIPLE])
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(probe)
        .envs(env)
        .env("RUSTC_BOOTSTRAP", "1")
        .status()
        .unwrap();
    assert!(status.success(), "building the probe failed with {status}");
    let program = target_dir.join(TRIPLE).join("release").join("tls-probe");
    Command::new(&program).output().unwrap()
}

fn main() {
    let scratch = tempfile::tempdir().unwrap();
    let probe = scratch.path().join("tls-probe");
    std::fs::create_dir_all(probe.join("src")).unwrap();
    std::fs::write(
        probe.join("Cargo.toml"),
        "[package]\nname = \"tls-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(probe.join("src/main.rs"), probe_source()).unwrap();
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| scratch.path().join("target"), Into::into)
        .join("android-tls-probe");

    let built = build_and_run(
        &probe,
        &target_dir.join("native"),
        &native_tls_cargo_args(TRIPLE),
    );
    // Each thread-local, first touched by each thread, counts one.
    let expected = THREAD_LOCALS;
    assert!(
        built.status.success(),
        "the program built with native thread-locals failed with {}: {}",
        built.status,
        String::from_utf8_lossy(&built.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&built.stdout).trim(),
        format!("touched {expected} and {expected}"),
    );

    let stock = build_and_run(&probe, &target_dir.join("stock"), &[]);
    assert!(
        !stock.status.success()
            && String::from_utf8_lossy(&stock.stderr).contains("out of TLS keys"),
        "rustc's own Android std runs out of pthread keys: {} {}",
        stock.status,
        String::from_utf8_lossy(&stock.stderr)
    );
}
