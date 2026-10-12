use std::env;
use std::fs::File;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `cmake` may take to configure: seconds normally, minutes on a
/// loaded host. Past it the configure is stuck, not slow.
const CONFIGURE_BOUND: Duration = Duration::from_secs(20 * 60);
/// How long building and installing the native kernels may take.
const BUILD_BOUND: Duration = Duration::from_secs(90 * 60);

/// Run `command` (a `cmake` step named `step`) to completion within `bound`,
/// its output written to `log`.
///
/// `cmake` and everything it runs (compiler probes, the linker) read nothing
/// and inherit nothing of this build: stdin is `/dev/null`, every descriptor
/// past stderr is closed when they start, and the `MAKEFLAGS` family is gone,
/// so no Cargo jobserver pipe or other descriptor this process holds can
/// reach them. `cmake` answers its own parallelism (`--parallel`). Past
/// `bound` its whole process group is killed and the build fails, naming
/// the log, instead of hanging.
fn checked(step: &str, command: &mut Command, log: &Path, bound: Duration) -> io::Result<()> {
    let output = File::create(log)?;
    command
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .env_remove("CARGO_MAKEFLAGS")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        // Its own process group, so a timeout reaches the probes it runs.
        .process_group(0);
    // SAFETY: the closure only makes raw syscalls (`close_range`, `fcntl`),
    // which are async-signal-safe, between fork and exec.
    unsafe {
        command.pre_exec(|| {
            close_inherited_descriptors();
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > bound {
            let group = libc::pid_t::try_from(child.id()).unwrap_or(0);
            if group > 0 {
                // SAFETY: signals the process group the child leads.
                unsafe {
                    libc::killpg(group, libc::SIGKILL);
                }
            }
            let _ = child.wait();
            show_tail(log);
            return Err(io::Error::other(format!(
                "speech CPU {step} did not finish within {} minutes; see {}",
                bound.as_secs() / 60,
                log.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if status.success() {
        Ok(())
    } else {
        show_tail(log);
        Err(io::Error::other(format!(
            "speech CPU {step} failed ({status}); see {}",
            log.display()
        )))
    }
}

/// The end of `log` on this build script's stderr, which Cargo shows when
/// the build fails.
fn show_tail(log: &Path) {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    for line in lines
        .get(lines.len().saturating_sub(80)..)
        .unwrap_or_default()
    {
        eprintln!("{line}");
    }
}

/// Mark every descriptor past stderr close-on-exec, so the program `exec`
/// starts holds none of them; the ones the standard library itself needs
/// until `exec` stay open until then.
fn close_inherited_descriptors() {
    // SAFETY: plain syscalls on this process's own descriptor table.
    let marked = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    };
    if marked == 0 {
        return;
    }
    // Kernels before 5.11 lack the flag: mark them one by one.
    // SAFETY: as above.
    let limit = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    let limit = libc::c_int::try_from(limit).unwrap_or(4096).clamp(3, 65536);
    for descriptor in 3..limit {
        // SAFETY: as above; an unused descriptor answers EBADF, ignored.
        unsafe {
            libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=native");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return Ok(());
    }
    if env::var_os("CARGO_FEATURE_WHISPER").is_none() {
        return Ok(());
    }
    let root = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").ok_or("Cargo manifest directory is missing")?,
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo output directory is missing")?);
    let build = output.join("speech-cpu");
    let destination = output.join("speech-install");
    let source = root.join("native/upstream/whisper.cpp");
    let compiler = cc::Build::new().cpp(true).get_compiler();
    let c_compiler = cc::Build::new().get_compiler();
    let flags = |tool: &cc::Tool| {
        tool.args()
            .iter()
            .map(|arg| format!("\"{}\"", arg.to_string_lossy().replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(" ")
    };
    checked(
        "configure",
        Command::new("cmake")
            .arg("-S")
            .arg(root.join("native"))
            .arg("-B")
            .arg(&build)
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .arg("-DCMAKE_POSITION_INDEPENDENT_CODE=ON")
            .arg("-DCMAKE_SYSTEM_NAME=Linux")
            .arg(format!(
                "-DCMAKE_SYSTEM_PROCESSOR={}",
                env::var("CARGO_CFG_TARGET_ARCH")?
            ))
            .arg(format!(
                "-DCMAKE_CXX_COMPILER={}",
                compiler.path().display()
            ))
            .arg(format!(
                "-DCMAKE_C_COMPILER={}",
                c_compiler.path().display()
            ))
            .arg(format!("-DCMAKE_CXX_FLAGS={}", flags(&compiler)))
            .arg(format!("-DCMAKE_C_FLAGS={}", flags(&c_compiler)))
            .arg(format!("-DTILO_WHISPER_ROOT={}", source.display()))
            .arg(format!("-DCMAKE_INSTALL_PREFIX={}", destination.display())),
        &output.join("speech-configure.log"),
        CONFIGURE_BOUND,
    )?;
    // Native kernels are one owned heavy build; respect the Cargo job budget
    // and leave CPU headroom for the desktop and other sessions.
    let jobs = env::var("NUM_JOBS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 2);
    checked(
        "build",
        Command::new("cmake")
            .arg("--build")
            .arg(&build)
            .arg("--parallel")
            .arg(jobs.to_string()),
        &output.join("speech-build.log"),
        BUILD_BOUND,
    )?;
    checked(
        "install",
        Command::new("cmake").arg("--install").arg(&build),
        &output.join("speech-install.log"),
        CONFIGURE_BOUND,
    )?;
    let mut bridge = cc::Build::new();
    bridge
        .cpp(true)
        .std("c++17")
        .include(source.join("include"))
        .include(source.join("ggml/include"))
        .file(root.join("native/speech.cpp"))
        .file(root.join("native/activity.cpp"));
    bridge.compile("tilo-speech-bridge");
    println!(
        "cargo:rustc-link-search=native={}",
        destination.join("lib").display()
    );
    for library in ["whisper", "ggml", "ggml-base", "ggml-cpu"] {
        println!("cargo:rustc-link-lib=static={library}");
    }
    Ok(())
}
